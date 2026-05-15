//! Non-secret Balanze settings.
//!
//! Lives at `directories::ProjectDirs::from("me", "oszkar", "Balanze").config_dir()/settings.json`
//! per AGENTS.md §2.1's filesystem layout rule. Reads on demand; writes are
//! atomic (tmp + rename) so a crash mid-write doesn't leave a half-written
//! file.
//!
//! **Secrets do not live here.** API keys go through `crates/keychain`. This
//! file is plaintext JSON; treat anything written here as visible to anyone
//! with read access to the user's home directory.
//!
//! Schema is versioned (currently `version: 1`). Adding a field: add it
//! `#[serde(default)]` so old files still parse. Removing/renaming a field
//! requires bumping the version and adding a migration step in `load_from`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Schema version. Bumped when a load-time migration is needed.
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub providers: ProviderSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSettings {
    /// User has configured an OpenAI Platform API key (in the keychain).
    /// The key itself is NOT stored in this file.
    #[serde(default)]
    pub openai_enabled: bool,
    /// Claude OAuth lookups always run when `~/.claude/.credentials.json`
    /// is present; this toggle exists so a user can disable polling without
    /// removing the credential file.
    #[serde(default = "default_true")]
    pub anthropic_enabled: bool,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            openai_enabled: false,
            anthropic_enabled: true,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            providers: ProviderSettings::default(),
        }
    }
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("unable to resolve a config directory for this user")]
    NoConfigDir,

    #[error("io error on {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("settings file at {path:?} is malformed: {reason}")]
    Malformed { path: PathBuf, reason: String },
}

/// Conventional settings.json path for this user. Lazy: doesn't create the
/// directory.
pub fn default_path() -> Result<PathBuf, SettingsError> {
    let pd = directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .ok_or(SettingsError::NoConfigDir)?;
    Ok(pd.config_dir().join("settings.json"))
}

/// Load settings from the conventional path, returning `Settings::default()`
/// if the file is missing. If the file is corrupt, returns `Malformed` so
/// the caller can decide whether to fail or fall back to defaults.
pub fn load() -> Result<Settings, SettingsError> {
    let path = default_path()?;
    load_from(&path)
}

/// Load settings from an explicit path. Used by tests and by any future
/// override path (e.g. `--config` CLI flag).
pub fn load_from(path: &Path) -> Result<Settings, SettingsError> {
    debug!(path = %path.display(), "settings: load");
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(path = %path.display(), "settings: file absent, returning defaults");
            return Ok(Settings::default());
        }
        Err(e) => {
            return Err(SettingsError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    let mut parsed: Settings =
        serde_json::from_slice(&bytes).map_err(|e| SettingsError::Malformed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    if parsed.version > SCHEMA_VERSION {
        warn!(
            seen = parsed.version,
            known = SCHEMA_VERSION,
            "settings: file written by newer Balanze; some fields may be ignored"
        );
    }
    if parsed.version == 0 {
        parsed.version = SCHEMA_VERSION;
    }
    Ok(parsed)
}

/// Save settings atomically: write to `<path>.tmp`, fsync, rename over `<path>`.
/// Creates parent directories as needed.
pub fn save(settings: &Settings) -> Result<(), SettingsError> {
    let path = default_path()?;
    save_to(settings, &path)
}

pub fn save_to(settings: &Settings, path: &Path) -> Result<(), SettingsError> {
    debug!(path = %path.display(), "settings: save");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SettingsError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| SettingsError::Malformed {
        path: path.to_path_buf(),
        reason: format!("serialization failed: {e}"),
    })?;

    let tmp = tmp_path(path);
    {
        let mut f = fs::File::create(&tmp).map_err(|e| SettingsError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        f.write_all(&bytes).map_err(|e| SettingsError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        f.sync_all().map_err(|e| SettingsError::Io {
            path: tmp.clone(),
            source: e,
        })?;
    }
    fs::rename(&tmp, path).map_err(|e| SettingsError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn tmp_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_current_schema_version() {
        let s = Settings::default();
        assert_eq!(s.version, SCHEMA_VERSION);
        assert!(!s.providers.openai_enabled);
        assert!(s.providers.anthropic_enabled);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let s = load_from(&path).expect("load");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.providers.openai_enabled = true;
        save_to(&s, &path).expect("save");
        let loaded = load_from(&path).expect("load");
        assert_eq!(s, loaded);
    }

    #[test]
    fn save_uses_atomic_write_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save_to(&Settings::default(), &path).expect("save");
        // After a successful save, the tmp file should NOT exist.
        let tmp = tmp_path(&path);
        assert!(!tmp.exists(), "leftover {tmp:?} after successful save");
        assert!(path.exists());
    }

    #[test]
    fn save_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("subdir")
            .join("settings.json");
        assert!(!path.parent().unwrap().exists());
        save_to(&Settings::default(), &path).expect("save");
        assert!(path.exists());
    }

    #[test]
    fn load_corrupt_file_returns_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{not valid json").unwrap();
        match load_from(&path) {
            Err(SettingsError::Malformed { path: p, reason }) => {
                assert_eq!(p, path);
                assert!(!reason.is_empty());
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn loads_minimal_file_with_only_version_field() {
        // Backwards-compat: a settings file written by an older Balanze with
        // only `{"version":1}` should fill in defaults for new fields.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"version":1}"#).unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(s.version, 1);
        assert!(s.providers.anthropic_enabled);
        assert!(!s.providers.openai_enabled);
    }

    #[test]
    fn loads_file_with_unknown_extra_fields() {
        // serde's default behavior is to ignore unknown fields, which is what
        // we want — a settings file written by a newer Balanze should still
        // load on an older binary, with the new fields dropped silently.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"providers":{"openai_enabled":true},"future_field":"x"}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert!(s.providers.openai_enabled);
    }

    #[test]
    fn explicit_version_zero_is_migrated_to_current() {
        // Distinct from the omitted-version case below: a file that
        // *explicitly* carries `version: 0` (the pre-versioning sentinel)
        // must be migrated up to the current schema on load. Exercises the
        // `parsed.version == 0` branch in load_from, which the
        // serde-defaulted (omitted) case never reaches.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":0,"providers":{"openai_enabled":true}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.version, SCHEMA_VERSION,
            "explicit version 0 must migrate to current"
        );
        assert!(
            s.providers.openai_enabled,
            "data preserved through migration"
        );
    }

    #[test]
    fn unset_version_field_treated_as_current() {
        // Older settings files may omit the version field entirely.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"providers":{"openai_enabled":true}}"#).unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(s.version, SCHEMA_VERSION);
        assert!(s.providers.openai_enabled);
    }
}
