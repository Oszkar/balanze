//! Coordinates Balanze's statusline backup with Claude Code's settings file.
//!
//! The two files cannot be committed atomically. This module therefore owns the
//! safe ordering: persist a displaced command before wiring Claude, roll that
//! backup back on a reported wire failure, and clear it only after restore has
//! succeeded. The Balanze settings lock stays held across the full workflow so
//! two cooperating Balanze processes cannot interleave these steps.

use std::path::Path;

use thiserror::Error;

use crate::{
    NON_STRING_STATUSLINE_COMMAND, StatuslineError, WireStatus, read_wire_status,
    restore_statusline, wire_statusline,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceOutcome {
    Wired,
    Replaced { displaced: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored(String),
    AlreadyRestored(String),
    Unwired,
    NothingToDo,
    Refused { occupying_command: String },
}

#[derive(Debug, Error)]
pub enum StatuslineWorkflowError {
    #[error(transparent)]
    Settings(#[from] settings::SettingsError),

    #[error(transparent)]
    Statusline(#[from] StatuslineError),

    #[error(
        "Claude statusLine write failed ({write}); restoring the prior Balanze backup also failed ({rollback})"
    )]
    Rollback {
        write: StatuslineError,
        rollback: settings::SettingsError,
    },
}

pub fn replace_statusline_with_backup(
    claude_settings_path: &Path,
    invocation: &str,
) -> Result<ReplaceOutcome, StatuslineWorkflowError> {
    let balanze_settings_path = settings::default_path()?;
    replace_statusline_with_backup_at(
        &balanze_settings_path,
        claude_settings_path,
        invocation,
        wire_statusline,
    )
}

fn replace_statusline_with_backup_at(
    balanze_settings_path: &Path,
    claude_settings_path: &Path,
    invocation: &str,
    write_claude: impl FnOnce(&Path, &str) -> Result<(), StatuslineError>,
) -> Result<ReplaceOutcome, StatuslineWorkflowError> {
    let mut transaction = settings::begin_update_at(balanze_settings_path)?;
    let prior_backup = transaction.settings().statusline.replaced_command.clone();
    let status = read_wire_status(claude_settings_path)?;

    let displaced = match &status {
        WireStatus::OccupiedBy(command) if command != NON_STRING_STATUSLINE_COMMAND => {
            transaction.settings_mut().statusline.replaced_command = Some(command.clone());
            transaction.publish()?;
            Some(command.clone())
        }
        _ => None,
    };

    if let Err(write) = write_claude(claude_settings_path, invocation) {
        if displaced.is_some() {
            transaction.settings_mut().statusline.replaced_command = prior_backup;
            if let Err(rollback) = transaction.publish() {
                return Err(StatuslineWorkflowError::Rollback { write, rollback });
            }
        }
        return Err(StatuslineWorkflowError::Statusline(write));
    }

    Ok(match status {
        WireStatus::OccupiedBy(_) => ReplaceOutcome::Replaced { displaced },
        WireStatus::Unwired | WireStatus::WiredToBalanze => ReplaceOutcome::Wired,
    })
}

pub fn restore_statusline_from_backup(
    claude_settings_path: &Path,
) -> Result<RestoreOutcome, StatuslineWorkflowError> {
    let balanze_settings_path = settings::default_path()?;
    restore_statusline_from_backup_at(&balanze_settings_path, claude_settings_path)
}

fn restore_statusline_from_backup_at(
    balanze_settings_path: &Path,
    claude_settings_path: &Path,
) -> Result<RestoreOutcome, StatuslineWorkflowError> {
    let mut transaction = settings::begin_update_at(balanze_settings_path)?;
    let previous = transaction.settings().statusline.replaced_command.clone();
    let status = read_wire_status(claude_settings_path)?;

    if let Some(previous) = previous {
        if let WireStatus::OccupiedBy(current) = &status {
            if current == &previous {
                transaction.settings_mut().statusline.replaced_command = None;
                transaction.publish()?;
                return Ok(RestoreOutcome::AlreadyRestored(previous));
            }
            return Ok(RestoreOutcome::Refused {
                occupying_command: current.clone(),
            });
        }

        if !restore_statusline(claude_settings_path, Some(&previous))? {
            let occupying_command = match read_wire_status(claude_settings_path)? {
                WireStatus::OccupiedBy(command) => command,
                WireStatus::Unwired => "<unwired>".to_string(),
                WireStatus::WiredToBalanze => "<Balanze>".to_string(),
            };
            return Ok(RestoreOutcome::Refused { occupying_command });
        }
        transaction.settings_mut().statusline.replaced_command = None;
        transaction.publish()?;
        return Ok(RestoreOutcome::Restored(previous));
    }

    match status {
        WireStatus::WiredToBalanze => {
            if restore_statusline(claude_settings_path, None)? {
                Ok(RestoreOutcome::Unwired)
            } else {
                Ok(RestoreOutcome::NothingToDo)
            }
        }
        WireStatus::Unwired | WireStatus::OccupiedBy(_) => Ok(RestoreOutcome::NothingToDo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_claude_settings(path: &Path, command: &str) {
        std::fs::write(
            path,
            format!(r#"{{"statusLine":{{"type":"command","command":"{command}"}}}}"#),
        )
        .unwrap();
    }

    fn save_balanze_settings(path: &Path, backup: Option<&str>) {
        let mut transaction = settings::begin_update_at(path).unwrap();
        transaction.settings_mut().statusline.replaced_command = backup.map(str::to_string);
        transaction.commit().unwrap();
    }

    #[test]
    fn replace_persists_backup_before_write_and_rolls_back_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let balanze = dir.path().join("balanze.json");
        let claude = dir.path().join("claude.json");
        save_balanze_settings(&balanze, Some("older-command"));
        write_claude_settings(&claude, "foreign-command");

        let error = replace_statusline_with_backup_at(
            &balanze,
            &claude,
            "balanze-cli statusline",
            |_, _| {
                assert_eq!(
                    settings::load_from(&balanze)
                        .unwrap()
                        .statusline
                        .replaced_command
                        .as_deref(),
                    Some("foreign-command")
                );
                Err(StatuslineError::SettingsIo {
                    path: claude.clone(),
                    source: std::io::Error::other("injected write failure"),
                })
            },
        )
        .unwrap_err();

        assert!(matches!(error, StatuslineWorkflowError::Statusline(_)));
        assert_eq!(
            settings::load_from(&balanze)
                .unwrap()
                .statusline
                .replaced_command
                .as_deref(),
            Some("older-command")
        );
        assert_eq!(
            read_wire_status(&claude).unwrap(),
            WireStatus::OccupiedBy("foreign-command".to_string())
        );
    }

    #[test]
    fn restore_reconciles_an_already_restored_command() {
        let dir = tempfile::tempdir().unwrap();
        let balanze = dir.path().join("balanze.json");
        let claude = dir.path().join("claude.json");
        save_balanze_settings(&balanze, Some("foreign-command"));
        write_claude_settings(&claude, "foreign-command");

        assert_eq!(
            restore_statusline_from_backup_at(&balanze, &claude).unwrap(),
            RestoreOutcome::AlreadyRestored("foreign-command".to_string())
        );
        assert!(
            settings::load_from(&balanze)
                .unwrap()
                .statusline
                .replaced_command
                .is_none()
        );
    }

    #[test]
    fn restore_refuses_unrelated_foreign_command_and_keeps_backup() {
        let dir = tempfile::tempdir().unwrap();
        let balanze = dir.path().join("balanze.json");
        let claude = dir.path().join("claude.json");
        save_balanze_settings(&balanze, Some("saved-command"));
        write_claude_settings(&claude, "new-foreign-command");

        assert_eq!(
            restore_statusline_from_backup_at(&balanze, &claude).unwrap(),
            RestoreOutcome::Refused {
                occupying_command: "new-foreign-command".to_string()
            }
        );
        assert_eq!(
            settings::load_from(&balanze)
                .unwrap()
                .statusline
                .replaced_command
                .as_deref(),
            Some("saved-command")
        );
    }
}
