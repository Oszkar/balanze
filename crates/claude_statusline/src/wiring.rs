//! Owns the `statusLine` stanza in Claude Code's `settings.json`.
//!
//! Mirrors the dual-responsibility pattern of `anthropic_oauth::credentials`:
//! this module is the ONLY code that reads/writes the `statusLine` key in
//! Claude's `settings.json`, exactly as `anthropic_oauth` is the only code
//! that reads/writes `~/.claude/.credentials.json`.
//!
//! Search order (first existing path wins):
//!   1. `$XDG_CONFIG_HOME/claude/settings.json` (if XDG_CONFIG_HOME is set)
//!   2. `~/.claude/settings.json` - legacy, still used on Windows + many macOS installs
//!   3. `~/.config/claude/settings.json` - Claude Code v1.0.30+ on some platforms
//!
//! `wire_statusline` is the ONLY writer and uses atomic tmp+rename, preserves
//! all other keys, and creates the file+parent dir if absent (AGENTS.md §3.4
//! pattern - no secret so no 0o600 requirement; plain ACL inheritance is fine).

use std::path::{Path, PathBuf};

use crate::errors::StatuslineError;

/// Canonical `statusLine.command` Balanze wires into Claude Code's
/// `settings.json`. Bare `balanze-cli` assumes it is on PATH (true after
/// `cargo install`). Shared by the CLI `setup` flow and the desktop Settings UI
/// so the two can't drift.
pub const STATUSLINE_INVOCATION: &str = "balanze-cli statusline";

/// Sentinel `OccupiedBy` payload for a `statusLine` whose `.command` is absent
/// or not a JSON string. It is not a runnable command, so callers must not back
/// it up as a displaced command to restore later.
pub const NON_STRING_STATUSLINE_COMMAND: &str = "<non-string statusLine>";

/// Whether the `statusLine` stanza is owned by Balanze, absent, or taken by
/// something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireStatus {
    /// No `settings.json` exists, or the file has no `statusLine` key.
    Unwired,
    /// `statusLine.command` contains both `"balanze-cli"` and `"statusline"`.
    WiredToBalanze,
    /// `statusLine.command` is a string but belongs to something else, or
    /// `statusLine` is present but `.command` is not a string.
    OccupiedBy(String),
}

/// Return the first existing settings path, or `SettingsMissing` listing every
/// path searched.
pub fn locate_settings_path() -> Result<PathBuf, StatuslineError> {
    let candidates = candidate_paths();
    for path in &candidates {
        if path.exists() {
            return Ok(path.clone());
        }
    }
    // candidate_paths() is empty only when no home/XDG var is set - unreachable on a normal desktop; mirrors anthropic_oauth::locate_credentials.
    Err(StatuslineError::SettingsMissing {
        searched: candidates,
    })
}

/// The canonical create location when no `settings.json` exists yet:
/// `~/.claude/settings.json` (USERPROFILE/HOME-based, like `credentials.rs`
/// `home_dir()`).
pub fn default_settings_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("settings.json")
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(xdg).join("claude").join("settings.json"));
    }
    if let Some(home) = home_dir() {
        out.push(home.join(".claude").join("settings.json"));
        out.push(home.join(".config").join("claude").join("settings.json"));
    }
    out
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Inspect the `statusLine` stanza in an existing (or possibly absent) file.
///
/// - `path` does not exist                              → `Ok(Unwired)`
/// - exists, valid JSON, no `"statusLine"` key          → `Ok(Unwired)`
/// - exists, `statusLine.command` contains both `"balanze-cli"` and `"statusline"`
///   → `Ok(WiredToBalanze)`
/// - exists, a different `statusLine.command` string → `Ok(OccupiedBy(cmd))`
/// - exists, `statusLine` present but `.command` is not a string
///   → `Ok(OccupiedBy("<non-string statusLine>"))`
/// - exists, not valid JSON / root not an object        → `Err(SettingsMalformed{…})`
/// - exists, io error reading                           → `Err(SettingsIo{…})`
pub fn read_wire_status(path: &Path) -> Result<WireStatus, StatuslineError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WireStatus::Unwired),
        Err(e) => {
            return Err(StatuslineError::SettingsIo {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    let root: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| StatuslineError::SettingsMalformed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

    // Root must be an object.
    if !root.is_object() {
        return Err(StatuslineError::SettingsMalformed {
            path: path.to_path_buf(),
            reason: "root is not a JSON object".to_string(),
        });
    }

    let status_line = match root.get("statusLine") {
        None => return Ok(WireStatus::Unwired),
        Some(v) => v,
    };

    match status_line.get("command").and_then(|v| v.as_str()) {
        // Substring heuristic: "are we already wired" guard, not a security boundary. A user wrapper containing both tokens would read as ours; acceptable for this CLI.
        Some(cmd) if cmd.contains("balanze-cli") && cmd.contains("statusline") => {
            Ok(WireStatus::WiredToBalanze)
        }
        Some(cmd) => Ok(WireStatus::OccupiedBy(cmd.to_string())),
        None => Ok(WireStatus::OccupiedBy(
            NON_STRING_STATUSLINE_COMMAND.to_string(),
        )),
    }
}

/// Set `settings.json`'s `"statusLine"` to
/// `{"type":"command","command":<invocation>}`, preserving every other key,
/// via atomic tmp+fsync+rename. If the file does not exist, creates it
/// (mkdir -p parent) as `{"statusLine":{...}}`.
///
/// Unconditionally sets `statusLine` - the no-clobber policy belongs to the
/// caller (Task 5 will call `read_wire_status` first). Safe to call repeatedly
/// (idempotent).
///
/// Note: the file is rewritten via `serde_json::to_vec_pretty`, so it is
/// normalized to pretty-printed JSON with object keys sorted
/// (`serde_json::Value` is a `BTreeMap`; the workspace does not enable
/// `preserve_order`). Semantically safe - Claude Code re-parses by key -
/// but a user who hand-ordered their settings.json will see keys sorted
/// after the first wire. Same accepted trade-off as `anthropic_oauth`'s
/// credentials write-back.
pub fn wire_statusline(path: &Path, invocation: &str) -> Result<(), StatuslineError> {
    // Load + parse existing content, or start with an empty object.
    // A single match on std::fs::read avoids the TOCTOU race of `if path.exists()
    // { std::fs::read(path) }` where a delete between the two calls would yield
    // a spurious SettingsIo instead of correctly creating the file.
    let mut root: serde_json::Value = match std::fs::read(path) {
        Ok(bytes) => {
            let v: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| StatuslineError::SettingsMalformed {
                    path: path.to_path_buf(),
                    reason: e.to_string(),
                })?;
            if !v.is_object() {
                return Err(StatuslineError::SettingsMalformed {
                    path: path.to_path_buf(),
                    reason: "root is not a JSON object".to_string(),
                });
            }
            v
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::Value::Object(serde_json::Map::new())
        }
        Err(e) => {
            return Err(StatuslineError::SettingsIo {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    // Mutate only the statusLine key; all other keys are untouched.
    root.as_object_mut().expect("guarded above").insert(
        "statusLine".to_string(),
        serde_json::json!({
            "type": "command",
            "command": invocation,
        }),
    );

    atomic_write_json(path, &root)
}

/// Restore a previously-displaced `statusLine.command`.
///
/// Writes the stanza only when doing so cannot clobber a foreign command Balanze
/// did not displace: `Some(cmd)` rewrites the stanza when it is Balanze's own
/// line or empty; `None` removes Balanze's own line. If a foreign command
/// currently occupies the stanza it is left untouched, upholding the
/// never-touch-foreign-config invariant symmetric with the no-clobber wire path.
/// Provider-agnostic: `cmd` is whatever string was displaced. Only Claude Code's
/// `settings.json` statusLine stanza is ever written.
///
/// Returns `true` if the stanza was written (restored or unwired), `false` if a
/// foreign command was left in place - so the caller can keep its backup.
pub fn restore_statusline(path: &Path, previous: Option<&str>) -> Result<bool, StatuslineError> {
    let status = read_wire_status(path)?;
    match previous {
        // A foreign command owns the stanza now - never overwrite it.
        Some(_) if matches!(status, WireStatus::OccupiedBy(_)) => Ok(false),
        Some(cmd) => {
            wire_statusline(path, cmd)?;
            Ok(true)
        }
        None if status == WireStatus::WiredToBalanze => {
            unwire_statusline(path)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Remove the `statusLine` stanza from `settings.json`, preserving every other
/// key, via the same atomic tmp+fsync+rename as [`wire_statusline`]. No-op
/// (returns `Ok`) if the file or the `statusLine` key is absent.
///
/// Like `wire_statusline`, this is unconditional at the crate level - the
/// no-clobber policy (only unwire a stanza we own) belongs to the caller, which
/// should check [`read_wire_status`] first so it never strips another tool's
/// `statusLine`.
pub fn unwire_statusline(path: &Path) -> Result<(), StatuslineError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        // Nothing to remove.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(StatuslineError::SettingsIo {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    let mut root: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| StatuslineError::SettingsMalformed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| StatuslineError::SettingsMalformed {
            path: path.to_path_buf(),
            reason: "root is not a JSON object".to_string(),
        })?;

    // Key already absent → nothing to write.
    if obj.remove("statusLine").is_none() {
        return Ok(());
    }

    atomic_write_json(path, &root)
}

/// Atomically write `root` as pretty JSON to `path`: mkdir -p the parent,
/// write a uniquely-named tmp, fsync, rename over the target, then fsync the
/// dir (Unix). Shared by [`wire_statusline`] and [`unwire_statusline`].
///
/// Normalizes to pretty-printed JSON with object keys sorted (`serde_json::Value`
/// is a `BTreeMap`; the workspace does not enable `preserve_order`). Semantically
/// safe - Claude Code re-parses by key - but a hand-ordered settings.json will
/// see keys sorted after the first write. Same accepted trade-off as
/// `anthropic_oauth`'s credentials write-back. No 0o600 requirement -
/// settings.json is not a secret file.
fn atomic_write_json(path: &Path, root: &serde_json::Value) -> Result<(), StatuslineError> {
    let serialized =
        serde_json::to_vec_pretty(root).map_err(|e| StatuslineError::SettingsMalformed {
            path: path.to_path_buf(),
            reason: format!("re-serialize: {e}"),
        })?;

    // Normalize the parent (a bare relative target's `parent()` is `Some("")`)
    // to exactly the directory `atomic_write` will use, so a relative target
    // doesn't fail at `create_dir_all("")` before the helper runs.
    let dir = atomic_file::resolve_parent(path);
    std::fs::create_dir_all(dir).map_err(|e| StatuslineError::SettingsIo {
        path: dir.to_path_buf(),
        source: e,
    })?;

    // Claude Code's settings.json is not a secret, but preserving its existing
    // mode across the replace (via `Permissions::Default`) is the right default.
    atomic_file::atomic_write(path, &serialized, atomic_file::Permissions::Default).map_err(
        |source| StatuslineError::SettingsIo {
            path: path.to_path_buf(),
            source,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVOCATION: &str = "balanze-cli statusline";

    fn write_settings(path: &Path, content: &str) {
        std::fs::write(path, content.as_bytes()).unwrap();
    }

    // ── read_wire_status ────────────────────────────────────────────────────

    #[test]
    fn read_wire_status_missing_file_is_unwired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // File does not exist.
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::Unwired);
    }

    #[test]
    fn read_wire_status_no_statusline_key_is_unwired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, r#"{"theme":"dark"}"#);
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::Unwired);
    }

    #[test]
    fn read_wire_status_wired_to_balanze() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(
            &path,
            r#"{"statusLine":{"type":"command","command":"balanze-cli statusline"}}"#,
        );
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::WiredToBalanze);
    }

    #[test]
    fn read_wire_status_detects_occupied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(
            &path,
            r#"{"statusLine":{"type":"command","command":"other-tool --status"}}"#,
        );
        match read_wire_status(&path).unwrap() {
            WireStatus::OccupiedBy(cmd) => assert_eq!(cmd, "other-tool --status"),
            other => panic!("expected OccupiedBy, got {other:?}"),
        }
    }

    #[test]
    fn read_wire_status_non_string_command_is_occupied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // statusLine present but .command is an integer, not a string.
        write_settings(&path, r#"{"statusLine":{"type":"command","command":42}}"#);
        match read_wire_status(&path).unwrap() {
            WireStatus::OccupiedBy(s) => assert_eq!(s, "<non-string statusLine>"),
            other => panic!("expected OccupiedBy(<non-string...>), got {other:?}"),
        }
    }

    #[test]
    fn read_wire_status_no_command_key_is_occupied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // statusLine present but has no .command key at all.
        write_settings(&path, r#"{"statusLine":{"type":"command"}}"#);
        match read_wire_status(&path).unwrap() {
            WireStatus::OccupiedBy(s) => assert_eq!(s, "<non-string statusLine>"),
            other => panic!("expected OccupiedBy(<non-string...>), got {other:?}"),
        }
    }

    #[test]
    fn malformed_settings_is_settings_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, "{not valid json");
        match read_wire_status(&path) {
            Err(StatuslineError::SettingsMalformed { path: p, reason }) => {
                assert_eq!(p, path);
                assert!(!reason.is_empty());
            }
            other => panic!("expected SettingsMalformed, got {other:?}"),
        }
    }

    #[test]
    fn read_wire_status_root_not_object_is_settings_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, "[1,2,3]");
        match read_wire_status(&path) {
            Err(StatuslineError::SettingsMalformed { .. }) => {}
            other => panic!("expected SettingsMalformed, got {other:?}"),
        }
    }

    // ── wire_statusline ──────────────────────────────────────────────────────

    #[test]
    fn wire_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, r#"{"theme":"dark","someOtherTool":{"active":true}}"#);

        wire_statusline(&path, INVOCATION).unwrap();

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["someOtherTool"]["active"], true);
        assert_eq!(v["statusLine"]["command"], INVOCATION);
        assert_eq!(v["statusLine"]["type"], "command");
    }

    #[test]
    fn wire_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        wire_statusline(&path, INVOCATION).unwrap();
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::WiredToBalanze);

        // Wire again - must still be WiredToBalanze and content must be stable.
        wire_statusline(&path, INVOCATION).unwrap();
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::WiredToBalanze);

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], INVOCATION);
    }

    #[test]
    fn default_path_or_missing_creates_minimal() {
        // Wire into a non-existent path under a tempdir - parent dir also absent.
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("subdir").join("settings.json");

        wire_statusline(&path, INVOCATION).unwrap();

        assert!(path.exists(), "file must be created");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], INVOCATION);
        assert_eq!(v["statusLine"]["type"], "command");
        // Only the statusLine key - nothing else.
        assert_eq!(
            v.as_object().unwrap().len(),
            1,
            "minimal file must have exactly one key"
        );
    }

    #[test]
    fn no_tmp_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, r#"{"a":1}"#);

        wire_statusline(&path, INVOCATION).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp files must be cleaned up: {leftovers:?}"
        );
    }

    #[test]
    fn wire_overwrites_existing_statusline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(
            &path,
            r#"{"statusLine":{"type":"command","command":"old-tool"}}"#,
        );

        wire_statusline(&path, INVOCATION).unwrap();

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], INVOCATION);
    }

    #[test]
    fn wire_malformed_existing_file_is_settings_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, "{not valid json");

        match wire_statusline(&path, INVOCATION) {
            Err(StatuslineError::SettingsMalformed { .. }) => {}
            other => panic!("expected SettingsMalformed, got {other:?}"),
        }
    }

    // ── unwire_statusline ─────────────────────────────────────────────────────

    #[test]
    fn unwire_removes_stanza_preserving_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(
            &path,
            r#"{"theme":"dark","statusLine":{"type":"command","command":"balanze-cli statusline"}}"#,
        );

        unwire_statusline(&path).unwrap();

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert!(v.get("statusLine").is_none(), "statusLine should be gone");
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::Unwired);
    }

    #[test]
    fn unwire_missing_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        unwire_statusline(&path).unwrap();
        assert!(!path.exists(), "must not create the file");
    }

    #[test]
    fn unwire_absent_key_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, r#"{"theme":"dark"}"#);
        unwire_statusline(&path).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
    }

    #[test]
    fn wire_then_unwire_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(&path, r#"{"keep":true}"#);

        wire_statusline(&path, INVOCATION).unwrap();
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::WiredToBalanze);

        unwire_statusline(&path).unwrap();
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::Unwired);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["keep"], true, "unrelated keys survive the roundtrip");
    }

    // ── restore_statusline ────────────────────────────────────────────────────

    #[test]
    fn restore_writes_previous_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        wire_statusline(&path, INVOCATION).unwrap(); // Balanze is wired
        assert!(
            restore_statusline(&path, Some("cship prompt")).unwrap(),
            "wrote the restored command"
        );
        assert_eq!(
            read_wire_status(&path).unwrap(),
            WireStatus::OccupiedBy("cship prompt".to_string())
        );
    }

    #[test]
    fn restore_none_unwires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        wire_statusline(&path, INVOCATION).unwrap();
        assert!(restore_statusline(&path, None).unwrap(), "wrote (unwired)");
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::Unwired);
    }

    #[test]
    fn restore_none_leaves_foreign_command_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A foreign tool owns the statusLine and Balanze never displaced it.
        wire_statusline(&path, "cship prompt").unwrap();
        // No backup + not Balanze-wired -> the foreign command must be left intact.
        assert!(!restore_statusline(&path, None).unwrap(), "did not write");
        assert_eq!(
            read_wire_status(&path).unwrap(),
            WireStatus::OccupiedBy("cship prompt".to_string())
        );
    }

    #[test]
    fn restore_some_leaves_foreign_command_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A different foreign tool now owns the statusLine (e.g. the user re-set
        // it after a Balanze replace). Restoring the backup must NOT clobber it.
        wire_statusline(&path, "other-tool prompt").unwrap();
        assert!(
            !restore_statusline(&path, Some("cship prompt")).unwrap(),
            "did not write over a foreign command"
        );
        assert_eq!(
            read_wire_status(&path).unwrap(),
            WireStatus::OccupiedBy("other-tool prompt".to_string())
        );
    }
}
