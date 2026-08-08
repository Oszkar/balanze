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

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};

pub mod statusline;
pub use statusline::StatuslineConfig;

const SCHEMA_VERSION: u32 = 2;
const SETTINGS_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const SETTINGS_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);

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

/// Field-level settings mutation intent received over IPC. Optional fields are
/// applied to the latest on-disk value under the settings lock, so an older UI
/// snapshot cannot overwrite an unrelated change made by another process.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SettingsPatch {
    #[serde(default)]
    pub providers: ProviderSettingsPatch,
    pub oauth_poll_interval_secs: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ProviderSettingsPatch {
    pub openai_enabled: Option<bool>,
    pub anthropic_enabled: Option<bool>,
    pub codex_enabled: Option<bool>,
}

impl SettingsPatch {
    pub fn apply(self, settings: &mut Settings) {
        if let Some(enabled) = self.providers.openai_enabled {
            settings.providers.openai_enabled = enabled;
        }
        if let Some(enabled) = self.providers.anthropic_enabled {
            settings.providers.anthropic_enabled = enabled;
        }
        if let Some(enabled) = self.providers.codex_enabled {
            settings.providers.codex_enabled = enabled;
        }
        if let Some(interval) = self.oauth_poll_interval_secs {
            settings.oauth_poll_interval_secs = interval;
        }
    }
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

    #[error("timed out after {timeout_ms} ms waiting for settings lock at {path:?}")]
    LockTimeout { path: PathBuf, timeout_ms: u128 },
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
/// defaulting a corrupt file here and then publishing an update would overwrite the
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

/// Raw-JSON form of [`migrate_statusline_lines`], used by [`normalize_on_disk_at`]
/// so the on-disk rewrite can preserve fields this binary does not model. Encodes
/// the same rule: only a `statusline.lines` byte-identical to
/// [`PREVIOUS_DEFAULT_LINES`] is rewritten to the current default; anything else
/// is left untouched. Keep this in lockstep with `migrate_statusline_lines`.
fn migrate_statusline_lines_value(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let matches_previous = obj
        .get("statusline")
        .and_then(|s| s.get("lines"))
        .and_then(|l| l.as_array())
        .is_some_and(|lines| {
            lines.len() == PREVIOUS_DEFAULT_LINES.len()
                && lines
                    .iter()
                    .zip(PREVIOUS_DEFAULT_LINES)
                    .all(|(v, expected)| v.as_str() == Some(expected))
        });
    if !matches_previous {
        return;
    }
    // `matches_previous` proved `statusline` is an object with a `lines` array.
    if let Some(statusline) = obj.get_mut("statusline").and_then(|s| s.as_object_mut()) {
        let new_lines = statusline::default_lines()
            .into_iter()
            .map(serde_json::Value::from)
            .collect();
        statusline.insert("lines".to_string(), serde_json::Value::Array(new_lines));
        debug!(
            "settings: migrated pre-schema-version-2 statusline lines to the current default (targeted JSON patch)"
        );
    }
}

/// Load settings for an update path. Identical to [`load`] on the happy path,
/// but the distinct name is a guard rail: a mutation caller must never
/// `.unwrap_or_default()` the result. Production mutations use [`begin_update`]
/// or [`update`], which call this only after acquiring the stable cross-process
/// lock. A missing file still yields `Settings::default()` (a first-ever save is
/// not data loss), but a `Malformed` or `Io` error is propagated so the caller
/// bails instead of resetting. See [`UPDATE_LOAD_HINT`] for the caller-facing
/// message.
pub fn load_for_update() -> Result<Settings, SettingsError> {
    let path = default_path()?;
    load_for_update_from(&path)
}

/// Explicit-path variant of [`load_for_update`], for tests and any future
/// `--config` override path.
pub fn load_for_update_from(path: &Path) -> Result<Settings, SettingsError> {
    load_from(path)
}

/// Shared caller-facing hint when [`load_for_update`] errors. Kept here so the
/// CLI mutation paths consistently distinguish malformed or unreadable settings
/// from temporary lock contention; callers append the propagated error for the
/// path + reason.
pub const UPDATE_LOAD_HINT: &str = "refusing to update settings.json; fix a malformed or unreadable file, or retry after the current settings operation finishes";

/// An exclusive, cross-process settings transaction. The lock is held from the
/// latest on-disk load until this value is dropped. Coupled workflows such as
/// keychain changes and statusline backup/restore may publish more than once
/// while retaining the same lock so another Balanze process cannot interleave.
pub struct SettingsTransaction {
    path: PathBuf,
    settings: Settings,
    _lock_file: File,
}

impl SettingsTransaction {
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Publish the transaction's current value while retaining the lock.
    pub fn publish(&self) -> Result<(), SettingsError> {
        save_to_unlocked(&self.settings, &self.path)
    }

    /// Publish and return the exact committed value. The lock is released before
    /// the caller receives the value because the transaction is consumed.
    pub fn commit(self) -> Result<Settings, SettingsError> {
        self.publish()?;
        Ok(self.settings)
    }
}

/// Acquire the conventional settings lock and reload the latest settings.
pub fn begin_update() -> Result<SettingsTransaction, SettingsError> {
    let path = default_path()?;
    begin_update_at(&path)
}

/// Explicit-path variant of [`begin_update`], used by deterministic tests.
pub fn begin_update_at(path: &Path) -> Result<SettingsTransaction, SettingsError> {
    begin_update_at_with_timeout(path, SETTINGS_LOCK_TIMEOUT)
}

fn begin_update_at_with_timeout(
    path: &Path,
    timeout: Duration,
) -> Result<SettingsTransaction, SettingsError> {
    let lock_file = acquire_settings_lock(path, timeout)?;
    let settings = load_for_update_from(path)?;
    Ok(SettingsTransaction {
        path: path.to_path_buf(),
        settings,
        _lock_file: lock_file,
    })
}

/// Reload, apply fallible field-level intent, and atomically publish under one
/// lock. If the mutation returns an error, the transaction is dropped without
/// publishing. `E: From<SettingsError>` lets callers use their own glue-layer
/// error type while preserving settings acquisition and publication failures.
pub fn update<E>(mutation: impl FnOnce(&mut Settings) -> Result<(), E>) -> Result<Settings, E>
where
    E: From<SettingsError>,
{
    let mut transaction = begin_update()?;
    mutation(transaction.settings_mut())?;
    Ok(transaction.commit()?)
}

/// Explicit-path variant of [`update`], used by deterministic tests.
pub fn update_at<E>(
    path: &Path,
    mutation: impl FnOnce(&mut Settings) -> Result<(), E>,
) -> Result<Settings, E>
where
    E: From<SettingsError>,
{
    let mut transaction = begin_update_at(path)?;
    mutation(transaction.settings_mut())?;
    Ok(transaction.commit()?)
}

fn acquire_settings_lock(path: &Path, timeout: Duration) -> Result<File, SettingsError> {
    let parent = atomic_file::resolve_parent(path);
    fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let lock_path = settings_lock_path(path);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| SettingsError::Io {
            path: lock_path.clone(),
            source,
        })?;

    let started = Instant::now();
    loop {
        match lock_file.try_lock() {
            Ok(()) => return Ok(lock_file),
            Err(TryLockError::WouldBlock) => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return Err(SettingsError::LockTimeout {
                        path: lock_path,
                        timeout_ms: timeout.as_millis(),
                    });
                }
                thread::sleep(SETTINGS_LOCK_RETRY_DELAY.min(timeout - elapsed));
            }
            Err(TryLockError::Error(source)) => {
                return Err(SettingsError::Io {
                    path: lock_path,
                    source,
                });
            }
        }
    }
}

fn settings_lock_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("settings.json"));
    file_name.push(".lock");
    path.with_file_name(file_name)
}

/// Persist a settings file written under an older schema version, so the
/// load-path migrations in [`load_from`] run once instead of on every load.
///
/// Without this, the in-memory version bump `load_from` applies only reaches
/// disk the next time something else happens to save the file. An existing
/// user whose file is already at the OLD default `statusline.lines` stays at
/// `version: 1` indefinitely if nothing else ever triggers a save, so
/// [`migrate_statusline_lines`] keeps stripping `{openai_cost}` back out on
/// every single load - including a user who reads the changelog and manually
/// re-adds the segment, since a hand re-add on a still-`version: 1` file
/// reproduces the exact previous-default string the migration matches on.
///
/// Returns `Ok(true)` if a write happened, `Ok(false)` if the file was
/// already current or absent. **Callers: the desktop app at startup only.**
/// Not the statusline path (`balanze-cli statusline`), which runs once per
/// prompt turn and must stay read-only per AGENTS.md §3.1.
pub fn normalize_on_disk() -> Result<bool, SettingsError> {
    let path = default_path()?;
    normalize_on_disk_at(&path)
}

/// Explicit-path variant of [`normalize_on_disk`], for tests and any future
/// `--config` override path.
///
/// Patches the raw JSON in place rather than round-tripping through [`Settings`]:
/// a full deserialize/reserialize would drop any field serde does not model, and
/// this rewrite runs automatically at startup, so it must not erase a
/// forward-compat field a newer Balanze wrote (or one a user hand-added). Only
/// `version` and a `statusline.lines` still pinned to the previous default are
/// touched; everything else is preserved byte-for-value.
///
/// A file that fails to parse as JSON is malformed and returns `Err(Malformed)`
/// without writing anything, so a corrupt file is never overwritten.
pub fn normalize_on_disk_at(path: &Path) -> Result<bool, SettingsError> {
    let _lock_file = acquire_settings_lock(path, SETTINGS_LOCK_TIMEOUT)?;
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(path = %path.display(), "settings: normalize_on_disk found no file, nothing to do");
            return Ok(false);
        }
        Err(e) => {
            return Err(SettingsError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    let mut doc: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| SettingsError::Malformed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    let Some(obj) = doc.as_object_mut() else {
        // A real settings.json is always a JSON object. Anything else we leave
        // untouched rather than risk clobbering a file we do not understand.
        debug!(path = %path.display(), "settings: normalize_on_disk found a non-object document, nothing to do");
        return Ok(false);
    };

    // An absent `version` is treated as already-current, exactly as the
    // `Settings::version` serde default would treat it.
    let version = obj
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::from(SCHEMA_VERSION));
    if version >= u64::from(SCHEMA_VERSION) {
        debug!(path = %path.display(), "settings: normalize_on_disk found current schema, nothing to do");
        return Ok(false);
    }

    migrate_statusline_lines_value(obj);
    obj.insert(
        "version".to_string(),
        serde_json::Value::from(SCHEMA_VERSION),
    );

    let out = serde_json::to_vec_pretty(&doc).map_err(|e| SettingsError::Malformed {
        path: path.to_path_buf(),
        reason: format!("serialization failed: {e}"),
    })?;
    write_json_atomic(path, &out)?;
    debug!(
        path = %path.display(),
        from = version,
        to = SCHEMA_VERSION,
        "settings: normalize_on_disk persisted the schema version bump"
    );
    Ok(true)
}

fn save_to_unlocked(settings: &Settings, path: &Path) -> Result<(), SettingsError> {
    debug!(path = %path.display(), "settings: save");
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| SettingsError::Malformed {
        path: path.to_path_buf(),
        reason: format!("serialization failed: {e}"),
    })?;
    write_json_atomic(path, &bytes)
}

/// Atomically write already-serialized JSON to `path` (fsync'd temp + rename,
/// plus a parent-dir fsync on unix), creating parent directories as needed.
/// Shared by locked full-settings publication and [`normalize_on_disk_at`] (a
/// targeted patch that preserves unknown fields). Callers must hold the stable
/// settings lock before invoking a full-settings publication.
fn write_json_atomic(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    // Normalize the parent (a bare relative target's `parent()` is `Some("")`)
    // to exactly the directory `atomic_write` will write into, so a relative
    // target doesn't fail here at `create_dir_all("")` before the helper runs.
    let parent = atomic_file::resolve_parent(path);
    fs::create_dir_all(parent).map_err(|e| SettingsError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;
    atomic_file::atomic_write(path, bytes, atomic_file::Permissions::Default).map_err(|source| {
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

    fn save_to(settings: &Settings, path: &Path) -> Result<(), SettingsError> {
        save_to_unlocked(settings, path)
    }

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
    fn normalize_on_disk_migrates_version_one_file_and_persists_the_bump() {
        // The remaining gap this function closes: a version-1 file with the
        // OLD default lines must land on disk at version 2 with the NEW
        // default lines, so the load-path migration in `load_from` never has
        // to reconsider this file again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"lines":["{model} {agent}","{context_bar} {cost} {usage} {codex} {openai_cost}"]}}"#,
        )
        .unwrap();

        let wrote = normalize_on_disk_at(&path).expect("normalize");
        assert!(wrote, "a version-1 file must be rewritten");

        let reloaded = load_from(&path).expect("reload");
        assert_eq!(
            reloaded.version, SCHEMA_VERSION,
            "on-disk version must be bumped to current"
        );
        assert_eq!(
            reloaded.statusline.lines,
            crate::statusline::default_lines(),
            "on-disk lines must be the current default after normalization"
        );
    }

    #[test]
    fn normalize_on_disk_is_idempotent_on_a_version_two_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"statusline":{"lines":["{model} {agent}","{context_bar} {cost} {usage} {codex} {openai_cost}"]}}"#,
        )
        .unwrap();

        assert!(normalize_on_disk_at(&path).expect("first call"));
        let after_first = fs::read(&path).unwrap();

        let wrote_again = normalize_on_disk_at(&path).expect("second call");
        assert!(
            !wrote_again,
            "a file already at the current schema version must not be rewritten"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            after_first,
            "second call must not touch the file"
        );
    }

    #[test]
    fn normalize_on_disk_leaves_a_deliberate_reenable_untouched() {
        // The regression guard for the whole fixed-point trap: a version-2
        // file whose lines were hand-edited to re-add {openai_cost} must
        // survive byte-for-byte. normalize_on_disk must not treat "already at
        // the current version" as an invitation to re-run the migration.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original: &[u8] = br#"{"version":2,"statusline":{"lines":["{model} {agent}","{context_bar} {cost} {usage} {codex} {openai_cost}"]}}"#;
        fs::write(&path, original).unwrap();

        let wrote = normalize_on_disk_at(&path).expect("normalize");
        assert!(!wrote, "a version-2 file must not be rewritten");
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "a deliberate re-enable on a version-2 file must be left byte-for-byte unchanged"
        );
    }

    #[test]
    fn normalize_on_disk_errors_on_malformed_file_and_leaves_it_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original: &[u8] = b"{ hand-edited into broken json ";
        fs::write(&path, original).unwrap();

        match normalize_on_disk_at(&path) {
            Err(SettingsError::Malformed { path: p, .. }) => assert_eq!(p, path),
            other => panic!("expected Malformed, got {other:?}"),
        }
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "a malformed file must never be overwritten by normalize_on_disk"
        );
    }

    #[test]
    fn normalize_on_disk_is_a_noop_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let wrote = normalize_on_disk_at(&path).expect("normalize");
        assert!(!wrote, "an absent file must not be created");
        assert!(!path.exists(), "normalize_on_disk must not create a file");
    }

    #[test]
    fn normalize_on_disk_preserves_unknown_fields_while_migrating() {
        // The automatic startup rewrite must not drop fields this binary does
        // not model: a targeted JSON patch touches only `version` and
        // `statusline.lines`, so a forward-compat field (written by a newer
        // Balanze) or a hand-added one survives the version bump. A full
        // deserialize/reserialize would silently erase them.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let raw = serde_json::json!({
            "version": 1,
            "statusline": {
                "theme": "dark",
                "lines": [
                    "{model} {agent}",
                    "{context_bar} {cost} {usage} {codex} {openai_cost}"
                ],
                "future_statusline_field": "keep me"
            },
            "future_top_level_field": { "nested": [1, 2, 3] }
        });
        fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

        let wrote = normalize_on_disk_at(&path).expect("normalize");
        assert!(wrote, "a version-1 file must be rewritten");

        let back: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["version"], serde_json::json!(2), "version bumped");
        assert_eq!(
            back["statusline"]["lines"],
            serde_json::json!(["{model} {agent}", "{context_bar} {cost} {usage} {codex}"]),
            "lines migrated to the current default"
        );
        assert_eq!(
            back["statusline"]["future_statusline_field"],
            serde_json::json!("keep me"),
            "unknown nested field must survive the rewrite"
        );
        assert_eq!(
            back["future_top_level_field"],
            serde_json::json!({ "nested": [1, 2, 3] }),
            "unknown top-level field must survive the rewrite"
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

    #[test]
    fn settings_patch_changes_only_present_fields() {
        let mut current = Settings::default();
        current.providers.openai_enabled = true;
        current.providers.anthropic_enabled = false;
        let patch = SettingsPatch {
            providers: ProviderSettingsPatch {
                codex_enabled: Some(false),
                ..Default::default()
            },
            oauth_poll_interval_secs: Some(900),
        };

        patch.apply(&mut current);

        assert!(current.providers.openai_enabled);
        assert!(!current.providers.anthropic_enabled);
        assert!(!current.providers.codex_enabled);
        assert_eq!(current.oauth_poll_interval_secs, 900);
    }

    #[test]
    fn settings_patch_deserializes_multi_field_intent() {
        let patch: SettingsPatch =
            serde_json::from_str(r#"{"providers":{"openai_enabled":false,"codex_enabled":false}}"#)
                .unwrap();

        assert_eq!(patch.providers.openai_enabled, Some(false));
        assert_eq!(patch.providers.anthropic_enabled, None);
        assert_eq!(patch.providers.codex_enabled, Some(false));
        assert_eq!(patch.oauth_poll_interval_secs, None);
    }

    #[test]
    fn failed_mutation_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save_to(&Settings::default(), &path).unwrap();

        let result = update_at(&path, |settings| {
            settings.providers.codex_enabled = false;
            Err(SettingsError::Malformed {
                path: path.clone(),
                reason: "injected mutation failure".to_string(),
            })
        });

        assert!(matches!(result, Err(SettingsError::Malformed { .. })));
        assert!(load_from(&path).unwrap().providers.codex_enabled);
    }

    #[test]
    fn separate_process_updates_preserve_independent_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save_to(&Settings::default(), &path).unwrap();

        let mut first = spawn_process_helper(dir.path(), "hold-first");
        wait_for_test_file(&dir.path().join("first-entered"));
        let mut second = spawn_process_helper(dir.path(), "write-second");
        wait_for_test_file(&dir.path().join("second-started"));
        thread::sleep(Duration::from_millis(50));
        assert!(
            !dir.path().join("second-done").exists(),
            "the second process must still be waiting for the first process's lock"
        );

        fs::write(dir.path().join("release-first"), b"go").unwrap();
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());

        let committed = load_from(&path).unwrap();
        assert!(!committed.providers.anthropic_enabled);
        assert!(!committed.providers.codex_enabled);
    }

    #[test]
    fn separate_process_contender_times_out_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save_to(&Settings::default(), &path).unwrap();

        let mut holder = spawn_process_helper(dir.path(), "hold-first");
        wait_for_test_file(&dir.path().join("first-entered"));
        let mut contender = spawn_process_helper(dir.path(), "timeout");
        assert!(contender.wait().unwrap().success());
        assert_eq!(
            fs::read_to_string(dir.path().join("timeout-result")).unwrap(),
            "timeout"
        );

        fs::write(dir.path().join("release-first"), b"go").unwrap();
        assert!(holder.wait().unwrap().success());
    }

    #[test]
    fn normalization_waits_for_a_separate_process_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"providers":{"anthropic_enabled":true,"codex_enabled":true}}"#,
        )
        .unwrap();

        let mut writer = spawn_process_helper(dir.path(), "hold-first");
        wait_for_test_file(&dir.path().join("first-entered"));
        let mut normalizer = spawn_process_helper(dir.path(), "normalize");
        wait_for_test_file(&dir.path().join("normalize-started"));
        thread::sleep(Duration::from_millis(50));
        assert!(!dir.path().join("normalize-done").exists());

        fs::write(dir.path().join("release-first"), b"go").unwrap();
        assert!(writer.wait().unwrap().success());
        assert!(normalizer.wait().unwrap().success());

        let committed = load_from(&path).unwrap();
        assert_eq!(committed.version, SCHEMA_VERSION);
        assert!(!committed.providers.anthropic_enabled);
    }

    #[test]
    fn settings_process_helper() {
        let Some(dir) = std::env::var_os("BALANZE_SETTINGS_PROCESS_TEST_DIR").map(PathBuf::from)
        else {
            return;
        };
        let role = std::env::var("BALANZE_SETTINGS_PROCESS_TEST_ROLE").unwrap();
        let path = dir.join("settings.json");
        match role.as_str() {
            "hold-first" => {
                let mut transaction = begin_update_at(&path).unwrap();
                fs::write(dir.join("first-entered"), b"ready").unwrap();
                wait_for_test_file(&dir.join("release-first"));
                transaction.settings_mut().providers.anthropic_enabled = false;
                transaction.commit().unwrap();
            }
            "write-second" => {
                fs::write(dir.join("second-started"), b"ready").unwrap();
                update_at(&path, |settings| {
                    settings.providers.codex_enabled = false;
                    Ok::<(), SettingsError>(())
                })
                .unwrap();
                fs::write(dir.join("second-done"), b"done").unwrap();
            }
            "timeout" => {
                let result = begin_update_at_with_timeout(&path, Duration::from_millis(50));
                let outcome = match result {
                    Err(SettingsError::LockTimeout { .. }) => "timeout",
                    Ok(_) => "acquired",
                    Err(error) => panic!("unexpected lock result: {error}"),
                };
                fs::write(dir.join("timeout-result"), outcome).unwrap();
            }
            "normalize" => {
                fs::write(dir.join("normalize-started"), b"ready").unwrap();
                normalize_on_disk_at(&path).unwrap();
                fs::write(dir.join("normalize-done"), b"done").unwrap();
            }
            other => panic!("unknown process-test role: {other}"),
        }
    }

    fn spawn_process_helper(dir: &Path, role: &str) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::settings_process_helper", "--nocapture"])
            .env("BALANZE_SETTINGS_PROCESS_TEST_DIR", dir)
            .env("BALANZE_SETTINGS_PROCESS_TEST_ROLE", role)
            .spawn()
            .unwrap()
    }

    fn wait_for_test_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
