//! Owns the `statusLine` stanza in Claude Code's `settings.json`.
//!
//! Mirrors the dual-responsibility pattern of `anthropic_oauth::credentials`:
//! this module is the ONLY code that reads/writes the `statusLine` key in
//! Claude's `settings.json`, exactly as `anthropic_oauth` is the only code
//! that reads/writes `~/.claude/.credentials.json`.
//!
//! Search order (first existing path wins):
//!   1. `$XDG_CONFIG_HOME/claude/settings.json` (if XDG_CONFIG_HOME is set)
//!   2. `~/.claude/settings.json` — legacy, still used on Windows + many macOS installs
//!   3. `~/.config/claude/settings.json` — Claude Code v1.0.30+ on some platforms
//!
//! `wire_statusline` is the ONLY writer and uses atomic tmp+rename, preserves
//! all other keys, and creates the file+parent dir if absent (AGENTS.md §3.4
//! pattern — no secret so no 0o600 requirement; plain ACL inheritance is fine).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::StatuslineError;

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
            })
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
        Some(cmd) if cmd.contains("balanze-cli") && cmd.contains("statusline") => {
            Ok(WireStatus::WiredToBalanze)
        }
        Some(cmd) => Ok(WireStatus::OccupiedBy(cmd.to_string())),
        None => Ok(WireStatus::OccupiedBy(
            "<non-string statusLine>".to_string(),
        )),
    }
}

/// Set `settings.json`'s `"statusLine"` to
/// `{"type":"command","command":<invocation>}`, preserving every other key,
/// via atomic tmp+fsync+rename. If the file does not exist, creates it
/// (mkdir -p parent) as `{"statusLine":{...}}`.
///
/// Unconditionally sets `statusLine` — the no-clobber policy belongs to the
/// caller (Task 5 will call `read_wire_status` first). Safe to call repeatedly
/// (idempotent).
pub fn wire_statusline(path: &Path, invocation: &str) -> Result<(), StatuslineError> {
    // Load + parse existing content, or start with an empty object.
    let mut root: serde_json::Value = if path.exists() {
        let bytes = std::fs::read(path).map_err(|e| StatuslineError::SettingsIo {
            path: path.to_path_buf(),
            source: e,
        })?;
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
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // Mutate only the statusLine key; all other keys are untouched.
    root.as_object_mut().expect("guarded above").insert(
        "statusLine".to_string(),
        serde_json::json!({
            "type": "command",
            "command": invocation,
        }),
    );

    let serialized =
        serde_json::to_vec_pretty(&root).map_err(|e| StatuslineError::SettingsMalformed {
            path: path.to_path_buf(),
            reason: format!("re-serialize: {e}"),
        })?;

    // Ensure parent directory exists.
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| StatuslineError::SettingsIo {
            path: dir.to_path_buf(),
            source: e,
        })?;
    }

    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    // Unique tmp name: pid + nanosecond timestamp + monotonic counter.
    // Avoids collisions on concurrent calls or PID reuse (mirrors credentials.rs).
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let tmp = dir.join(format!(
        "settings.json.balanze-{}-{}-{}.tmp",
        std::process::id(),
        nanos,
        seq,
    ));

    // Write to tmp, fsync, then atomically rename over the final path.
    // No 0o600 mode requirement — settings.json is not a secret file.
    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create_new(&tmp)?;
        f.write_all(&serialized)?;
        // fsync before rename: crash between write and rename cannot lose data.
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(StatuslineError::SettingsIo {
            path: tmp,
            source: e,
        });
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StatuslineError::SettingsIo {
            path: path.to_path_buf(),
            source: e,
        }
    })?;

    // Unix: fsync the parent directory so the rename itself is durable.
    // Best-effort — dir-fsync failure must not fail the write since data is
    // already renamed into place.
    #[cfg(unix)]
    {
        let _ = std::fs::File::open(dir).and_then(|f| f.sync_all());
    }

    Ok(())
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

        // Wire again — must still be WiredToBalanze and content must be stable.
        wire_statusline(&path, INVOCATION).unwrap();
        assert_eq!(read_wire_status(&path).unwrap(), WireStatus::WiredToBalanze);

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], INVOCATION);
    }

    #[test]
    fn default_path_or_missing_creates_minimal() {
        // Wire into a non-existent path under a tempdir — parent dir also absent.
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("subdir").join("settings.json");

        wire_statusline(&path, INVOCATION).unwrap();

        assert!(path.exists(), "file must be created");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], INVOCATION);
        assert_eq!(v["statusLine"]["type"], "command");
        // Only the statusLine key — nothing else.
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
}
