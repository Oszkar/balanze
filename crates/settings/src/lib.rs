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
//! Schema is versioned (currently `version: 2`). Adding a field: add it
//! `#[serde(default)]` so old files still parse. Removing/renaming a field
//! requires bumping the version and adding a migration step in `load_from`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};

pub mod statusline;
pub use statusline::StatuslineConfig;

const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Schema version. Bumped when a load-time migration is needed.
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub providers: ProviderSettings,
    /// Cadence (seconds) for the watcher's OAuth + OpenAI pollers.
    /// Default 300 - the §3.1 5-min API-politeness floor for provider
    /// usage/billing endpoints. Each poller (`watcher::tasks::oauth_poll`
    /// and `watcher::tasks::openai_poll`) clamps to a 300s minimum inside
    /// its own `spawn`, so a corrupt or malicious `settings.json` cannot
    /// drive the cadence below the floor regardless of what value lands
    /// here. Higher values are honored as-is.
    #[serde(default = "default_poll_interval")]
    pub oauth_poll_interval_secs: u32,
    /// True once the first-run welcome (auto-open popover + OS notification) has
    /// been shown. Backend-owned first-run state, not a user setting: the Tauri
    /// host sets it on first launch, and `set_settings` preserves it across
    /// frontend writes so a provider toggle never re-triggers the welcome.
    /// serde-default false so a fresh install (and older files) get it once.
    #[serde(default)]
    pub seen_welcome: bool,
    /// Statusline display configuration (segments, styles, thresholds, theme).
    /// Additive serde-default: an older settings.json gets the curated default
    /// (no schema version bump). Consumed by the `statusline_render` crate.
    #[serde(default)]
    pub statusline: StatuslineConfig,
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
    /// Codex (`~/.codex/sessions`) quota scanning. On by default; lets a user
    /// who doesn't use Codex stop the scan (and its cell) without uninstalling.
    #[serde(default = "default_true")]
    pub codex_enabled: bool,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            openai_enabled: false,
            anthropic_enabled: true,
            codex_enabled: true,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            providers: ProviderSettings::default(),
            oauth_poll_interval_secs: default_poll_interval(),
            seen_welcome: false,
            statusline: StatuslineConfig::default(),
        }
    }
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

fn default_poll_interval() -> u32 {
    300
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
///
/// `BALANZE_CONFIG_DIR_OVERRIDE` is intended for tests that need an isolated
/// config directory, mirroring `BALANZE_DATA_DIR_OVERRIDE` and
/// `BALANZE_CACHE_DIR_OVERRIDE`.
pub fn default_path() -> Result<PathBuf, SettingsError> {
    if let Ok(dir) = std::env::var("BALANZE_CONFIG_DIR_OVERRIDE") {
        return Ok(PathBuf::from(dir).join("settings.json"));
    }
    let pd = project_dirs().ok_or(SettingsError::NoConfigDir)?;
    Ok(pd.config_dir().join("settings.json"))
}

/// Statusline bridge file path for this user. Lazy: doesn't create the
/// directory.
///
/// `BALANZE_DATA_DIR_OVERRIDE` is intended for tests that need an isolated
/// bridge file location.
pub fn statusline_snapshot_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(PathBuf::from(env_path).join("statusline.snapshot.json"));
    }
    project_dirs().map(|d| d.data_dir().join("statusline.snapshot.json"))
}

/// Log file directory for this user (`<data_dir>/logs`). Lazy: doesn't create
/// the directory - the `tracing-appender` rolling writer creates it on first
/// write.
///
/// `BALANZE_DATA_DIR_OVERRIDE` is intended for tests that need an isolated
/// log directory (same override [`statusline_snapshot_path`] honors).
pub fn log_dir() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(PathBuf::from(env_path).join("logs"));
    }
    project_dirs().map(|d| d.data_dir().join("logs"))
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
}

/// Load settings from the conventional path, returning `Settings::default()`
/// if the file is missing. If the file is corrupt, returns `Malformed` so
/// the caller can decide whether to fail or fall back to defaults.
pub fn load() -> Result<Settings, SettingsError> {
    let path = default_path()?;
    load_from(&path)
}

/// Load settings, falling back to `Settings::default()` on ANY error (missing,
/// malformed, or unreadable) with a `warn`. For read-only consumers - the Tauri
/// watcher supervisor and `balanze-cli watch` - where proceeding on defaults is
/// correct. **Save-path callers must use [`load_for_update`] instead**: silently
/// defaulting a corrupt file here and then [`save`]-ing would overwrite the
/// user's real settings (including the `statusline.replaced_command` backup).
pub fn load_or_default() -> Settings {
    load().unwrap_or_else(|e| {
        warn!("settings load failed ({e}); using defaults");
        Settings::default()
    })
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
    // Migrations (including the version-0 pre-versioning sentinel) run only
    // for files older than the current schema, then the in-memory version is
    // normalized to SCHEMA_VERSION so a subsequent save persists the bump and
    // no migration in this block reconsiders the file again.
    if parsed.version < SCHEMA_VERSION {
        migrate_statusline_lines(&mut parsed);
        parsed.version = SCHEMA_VERSION;
    }
    Ok(parsed)
}

/// The statusline default line templates from before the `{openai_cost}`
/// segment left the default line. Kept only so [`migrate_statusline_lines`]
/// can recognize a persisted value that still matches this stale default.
/// A `const` array of `&str` (not `String`) so the comparison in
/// [`migrate_statusline_lines`] allocates nothing.
const PREVIOUS_DEFAULT_LINES: [&str; 2] = [
    "{model} {agent}",
    "{context_bar} {cost} {usage} {codex} {openai_cost}",
];

/// Load-path migration, gated to files written before schema version 2:
/// `StatuslineConfig.lines` is always serialized into `settings.json`, so any
/// file saved before the `{openai_cost}` segment left the default line has
/// that old default pinned literally - for those users the new default never
/// takes effect on its own. If a `version < 2` file's persisted value is
/// byte-identical to the previous default, this replaces it with the current
/// default (`statusline::default_lines`). A customized value is definitionally
/// not byte-identical to the previous default, so it is never touched.
///
/// This only fires once per file: `load_from` normalizes `version` to
/// `SCHEMA_VERSION` immediately after calling this, and the next [`save`]
/// persists that bump. So a user who deliberately hand-edits `statusline.lines`
/// back to include `{openai_cost}` - even if the resulting line is
/// byte-identical to `PREVIOUS_DEFAULT_LINES` - keeps it: their file is
/// already at `version: 2` and this function is not called for it. The
/// unversioned form of this migration used to be a fixed-point trap - the
/// documented re-enable path (append ` {openai_cost}` to the current default)
/// produced a string byte-identical to the previous default, so the very next
/// load stripped it right back out.
///
/// Deletion criterion: once no `version < 2` file is expected in the wild
/// (i.e. every user has loaded Balanze at least once past this change), this
/// function, [`PREVIOUS_DEFAULT_LINES`], and its call site can be removed.
fn migrate_statusline_lines(settings: &mut Settings) {
    if settings
        .statusline
        .lines
        .iter()
        .map(String::as_str)
        .eq(PREVIOUS_DEFAULT_LINES.iter().copied())
    {
        debug!(
            "settings: migrated pre-schema-version-2 statusline lines from the previous default to the current default"
        );
        settings.statusline.lines = statusline::default_lines();
    }
}

/// Load settings for a read-modify-**save** path. Identical to [`load`] on the
/// happy path, but the distinct name is a guard rail: a save-path caller must
/// never `.unwrap_or_default()` the result. A missing file still yields
/// `Settings::default()` (a first-ever save is not data loss), but a `Malformed`
/// or `Io` error is propagated so the caller bails instead of resetting. If a
/// caller collapsed a corrupt file to defaults here, the following [`save`]
/// would overwrite the user's real settings - including the
/// `statusline.replaced_command` backup - with a blank default, silently and
/// unrecoverably. See [`UPDATE_LOAD_HINT`] for the caller-facing message.
pub fn load_for_update() -> Result<Settings, SettingsError> {
    let path = default_path()?;
    load_for_update_from(&path)
}

/// Explicit-path variant of [`load_for_update`], for tests and any future
/// `--config` override path.
pub fn load_for_update_from(path: &Path) -> Result<Settings, SettingsError> {
    load_from(path)
}

/// Shared caller-facing hint when [`load_for_update`] errors: a save-path
/// caller refuses to overwrite a malformed/unreadable `settings.json` with
/// defaults. Kept here so the CLI and the Tauri commands surface one consistent
/// message; callers append the propagated error for the path + reason.
pub const UPDATE_LOAD_HINT: &str =
    "refusing to overwrite settings.json with defaults; fix or remove it and retry";

/// Save settings atomically via the shared `atomic_file` helper (fsync'd temp +
/// rename, plus a parent-dir fsync on unix). Creates parent directories as needed.
pub fn save(settings: &Settings) -> Result<(), SettingsError> {
    let path = default_path()?;
    save_to(settings, &path)
}

pub fn save_to(settings: &Settings, path: &Path) -> Result<(), SettingsError> {
    debug!(path = %path.display(), "settings: save");
    // Normalize the parent (a bare relative target's `parent()` is `Some("")`)
    // to exactly the directory `atomic_write` will write into, so a relative
    // target doesn't fail here at `create_dir_all("")` before the helper runs.
    let parent = atomic_file::resolve_parent(path);
    fs::create_dir_all(parent).map_err(|e| SettingsError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;

    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| SettingsError::Malformed {
        path: path.to_path_buf(),
        reason: format!("serialization failed: {e}"),
    })?;

    atomic_file::atomic_write(path, &bytes, atomic_file::Permissions::Default).map_err(|source| {
        SettingsError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes env-mutating tests in this module. `cargo nextest` runs each
    /// test in its own process, but plain `cargo test` shares one, so the lock
    /// keeps both runners honest.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_settings_have_current_schema_version() {
        let s = Settings::default();
        assert_eq!(s.version, SCHEMA_VERSION);
        assert!(!s.providers.openai_enabled);
        assert!(s.providers.anthropic_enabled);
        assert!(s.providers.codex_enabled);
    }

    #[test]
    fn codex_enabled_defaults_true_when_absent() {
        // Old settings.json written before codex_enabled existed must default
        // it to true (no version bump - additive serde-default field).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"providers":{"openai_enabled":false,"anthropic_enabled":true}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert!(
            s.providers.codex_enabled,
            "absent codex_enabled must default true"
        );
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
        assert!(path.exists());
        // A successful save leaves no temp files behind (atomic_file cleans up
        // its unique `*.tmp` on both the success and failure paths).
        let leftover_tmp = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!leftover_tmp, "leftover .tmp file after successful save");
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
    fn statusline_snapshot_path_honors_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: this env-mutating test is serialized via ENV_LOCK; the
        // override is test-only and removed before assertions run.
        unsafe { std::env::set_var("BALANZE_DATA_DIR_OVERRIDE", dir.path()) };
        let path = statusline_snapshot_path();
        unsafe { std::env::remove_var("BALANZE_DATA_DIR_OVERRIDE") };

        assert_eq!(path, Some(dir.path().join("statusline.snapshot.json")));
    }

    #[test]
    fn log_dir_honors_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: this env-mutating test is serialized via ENV_LOCK; the
        // override is test-only and removed before assertions run.
        unsafe { std::env::set_var("BALANZE_DATA_DIR_OVERRIDE", dir.path()) };
        let path = log_dir();
        unsafe { std::env::remove_var("BALANZE_DATA_DIR_OVERRIDE") };

        assert_eq!(path, Some(dir.path().join("logs")));
    }

    #[test]
    fn default_path_honors_config_dir_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK serializes env-mutating tests in this module; restored below.
        unsafe { std::env::set_var("BALANZE_CONFIG_DIR_OVERRIDE", dir.path()) };
        let p = default_path().expect("path");
        assert_eq!(p, dir.path().join("settings.json"));
        unsafe { std::env::remove_var("BALANZE_CONFIG_DIR_OVERRIDE") };
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
    fn load_for_update_errors_on_malformed_and_leaves_file_intact() {
        // A read-modify-SAVE path must never collapse a corrupt file to
        // defaults: doing so lets the following save() overwrite the user's
        // real settings (incl. the statusline replaced_command backup) with a
        // blank default. load_for_update must error and touch nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original: &[u8] = b"{ hand-edited into broken json ";
        fs::write(&path, original).unwrap();
        match load_for_update_from(&path) {
            Err(SettingsError::Malformed { path: p, .. }) => assert_eq!(p, path),
            other => panic!("expected Malformed, got {other:?}"),
        }
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "load_for_update must leave the corrupt file byte-for-byte intact"
        );
    }

    #[test]
    fn load_for_update_defaults_when_file_missing() {
        // A missing file is not corruption - a first-ever save is legitimate,
        // so update paths still get defaults here (this is the one case where
        // the old unwrap_or_default() was a correct no-op).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let s = load_for_update_from(&path).expect("missing file must default");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn load_for_update_loads_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.providers.openai_enabled = true;
        s.statusline.replaced_command = Some("original --statusline".to_string());
        save_to(&s, &path).expect("save");
        assert_eq!(load_for_update_from(&path).expect("load"), s);
    }

    #[test]
    fn loads_minimal_file_with_only_version_field() {
        // Backwards-compat: a settings file written by an older Balanze with
        // only `{"version":1}` should fill in defaults for new fields. A
        // version-1 file is below SCHEMA_VERSION, so load_from also bumps the
        // in-memory version to current - see `loads_minimal_file...` and the
        // SCHEMA_VERSION migration gate in `load_from`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"version":1}"#).unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.version, SCHEMA_VERSION,
            "version 1 must migrate to current"
        );
        assert!(s.providers.anthropic_enabled);
        assert!(!s.providers.openai_enabled);
    }

    #[test]
    fn loads_file_with_unknown_extra_fields() {
        // serde's default behavior is to ignore unknown fields, which is what
        // we want - a settings file written by a newer Balanze should still
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
        // `parsed.version < SCHEMA_VERSION` branch in load_from, which the
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

    #[test]
    fn oauth_poll_interval_defaults_to_300_when_absent() {
        // Old settings.json without the field must deserialize with the 300s default.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // File only has version + providers - no oauth_poll_interval_secs.
        fs::write(
            &path,
            br#"{"version":1,"providers":{"openai_enabled":false}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.oauth_poll_interval_secs, 300,
            "missing oauth_poll_interval_secs must default to 300"
        );
    }

    #[test]
    fn oauth_poll_interval_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let s = Settings {
            oauth_poll_interval_secs: 600,
            ..Default::default()
        };
        save_to(&s, &path).expect("save");
        let loaded = load_from(&path).expect("load");
        assert_eq!(loaded.oauth_poll_interval_secs, 600);
    }

    #[test]
    fn statusline_defaults_are_curated() {
        let c = crate::statusline::StatuslineConfig::default();
        assert_eq!(c.theme, "dark");
        assert!(!c.lines.is_empty(), "default lines present");
        assert!(c.segments.usage.show_pace);
        assert!(c.segments.usage.show_reset);
        assert_eq!(c.segments.cost.warn_micro_usd, 2_000_000);
        assert_eq!(c.segments.cost.critical_micro_usd, 5_000_000);
        assert_eq!(c.segments.context_bar.warn, 40);
        assert_eq!(c.segments.context_bar.critical, 70);
        assert_eq!(c.segments.usage.warn, 70);
        assert_eq!(c.segments.usage.critical, 90);
    }

    #[test]
    fn statusline_absent_defaults_to_curated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"providers":{"openai_enabled":false,"anthropic_enabled":true,"codex_enabled":true}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(s.statusline, crate::statusline::StatuslineConfig::default());
    }

    #[test]
    fn statusline_partial_segment_override_keeps_curated_thresholds() {
        // Overriding only ONE sub-field of a segment must still fill that
        // segment's curated thresholds (serde fills absent fields from each
        // field's serde-default, not the struct Default).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"segments":{"cost":{"style":"fg:#aabbcc"},"context_bar":{"style":"fg:#ddeeff"}}}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(s.statusline.segments.cost.warn_micro_usd, 2_000_000);
        assert_eq!(s.statusline.segments.cost.critical_micro_usd, 5_000_000);
        assert_eq!(s.statusline.segments.cost.style, "fg:#aabbcc");
        assert_eq!(s.statusline.segments.context_bar.warn, 40);
        assert_eq!(s.statusline.segments.context_bar.critical, 70);
    }

    #[test]
    fn statusline_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.statusline.theme = "light".to_string();
        s.statusline.segments.cost.warn_micro_usd = 9_000_000;
        save_to(&s, &path).expect("save");
        let loaded = load_from(&path).expect("load");
        assert_eq!(s, loaded);
    }

    #[test]
    fn migrates_previous_default_statusline_lines_to_current_default() {
        // A version-1 settings.json saved before the `{openai_cost}` segment
        // left the default line pins the OLD default literally. On load it
        // must be replaced with the CURRENT default so the segment (and the
        // OpenAI billing-API polling it demand-gates) turns off for anyone
        // who never customized `statusline.lines`, and the file's version
        // must be bumped so the migration does not reconsider it again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"lines":["{model} {agent}","{context_bar} {cost} {usage} {codex} {openai_cost}"]}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.statusline.lines,
            crate::statusline::default_lines(),
            "old default lines must be migrated to the current default"
        );
        assert_eq!(
            s.version, SCHEMA_VERSION,
            "a migrated file must be normalized to the current schema version"
        );
    }

    #[test]
    fn reenabled_openai_cost_on_schema_version_2_survives_migration() {
        // The critical regression: a user who reads the changelog and hand-
        // appends ` {openai_cost}` back onto the current default line ends up
        // with a `lines` value that is byte-identical to the OLD default. If
        // the migration were unversioned, the very next load would strip it
        // right back out - a fixed-point trap. Because this file is already
        // at schema version 2, migrate_statusline_lines must not even run for
        // it, so the hand-edit sticks.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":2,"statusline":{"lines":["{model} {agent}","{context_bar} {cost} {usage} {codex} {openai_cost}"]}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.statusline.lines,
            vec![
                "{model} {agent}".to_string(),
                "{context_bar} {cost} {usage} {codex} {openai_cost}".to_string(),
            ],
            "a deliberate re-add of {{openai_cost}} on a version-2 file must survive"
        );
        assert_eq!(s.version, SCHEMA_VERSION);
    }

    #[test]
    fn near_miss_double_space_line_survives_migration() {
        // The migration comparison must be byte-exact, not a trim/normalize
        // match: a double space is a different string from the previous
        // default and must never be touched, even though this file is a
        // version-1 file where the migration is eligible to fire.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"lines":["{model} {agent}","{context_bar}  {cost} {usage} {codex} {openai_cost}"]}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.statusline.lines,
            vec![
                "{model} {agent}".to_string(),
                "{context_bar}  {cost} {usage} {codex} {openai_cost}".to_string(),
            ],
            "a near-miss (double space) line must not be treated as the previous default"
        );
    }

    #[test]
    fn near_miss_second_line_only_survives_migration() {
        // Same near-miss guard, but for the shape of the previous default
        // rather than its spacing: a single-line file that matches only the
        // previous default's SECOND line is not the previous default and
        // must survive, even though this file is a version-1 file where the
        // migration is eligible to fire.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"lines":["{context_bar} {cost} {usage} {codex} {openai_cost}"]}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.statusline.lines,
            vec!["{context_bar} {cost} {usage} {codex} {openai_cost}".to_string()],
            "a near-miss (second-default-line-only) file must not be treated as the previous default"
        );
    }

    #[test]
    fn customized_statusline_lines_are_not_touched_by_migration() {
        // A user-customized `statusline.lines` is definitionally not
        // byte-identical to the previous default, so the migration must leave
        // it alone - including when it deliberately keeps `{openai_cost}`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"lines":["{usage} {openai_cost}"]}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(
            s.statusline.lines,
            vec!["{usage} {openai_cost}".to_string()],
            "customized lines must survive the migration untouched"
        );
    }

    #[test]
    fn already_migrated_statusline_lines_are_left_unchanged() {
        // The migration must be a no-op once a file already carries the
        // current default AND is already at the current schema version (e.g.
        // a file this migration already rewrote once, or a fresh save).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let current = crate::statusline::default_lines();
        let contents = serde_json::json!({
            "version": SCHEMA_VERSION,
            "statusline": { "lines": current },
        });
        fs::write(&path, serde_json::to_vec(&contents).unwrap()).unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(s.statusline.lines, current);
        assert_eq!(s.version, SCHEMA_VERSION);
    }

    #[test]
    fn load_for_update_from_migrates_statusline_lines_too() {
        // The migration must apply on the read-modify-save path as well -
        // otherwise a settings save right after load would persist the stale
        // old default (and the stale version) straight back to disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"lines":["{model} {agent}","{context_bar} {cost} {usage} {codex} {openai_cost}"]}}"#,
        )
        .unwrap();
        let s = load_for_update_from(&path).expect("load");
        assert_eq!(
            s.statusline.lines,
            crate::statusline::default_lines(),
            "load_for_update_from must apply the migration too"
        );
        assert_eq!(
            s.version, SCHEMA_VERSION,
            "load_for_update_from must persist the version bump so a subsequent save doesn't write the old lines back"
        );
    }

    #[test]
    fn seen_welcome_defaults_false_and_roundtrips() {
        // Fresh install + older files (absent field) must default false so the
        // first-run welcome shows exactly once.
        assert!(!Settings::default().seen_welcome);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"providers":{"openai_enabled":false}}"#,
        )
        .unwrap();
        assert!(
            !load_from(&path).unwrap().seen_welcome,
            "absent seen_welcome must default false"
        );
        let s = Settings {
            seen_welcome: true,
            ..Default::default()
        };
        save_to(&s, &path).unwrap();
        assert!(
            load_from(&path).unwrap().seen_welcome,
            "true must roundtrip"
        );
    }
}
