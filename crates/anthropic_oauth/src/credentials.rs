//! Locate and load Claude Code's credentials file.
//!
//! Search order (first existing path wins):
//!   1. `$XDG_CONFIG_HOME/claude/.credentials.json` (if XDG_CONFIG_HOME is set)
//!   2. `~/.claude/.credentials.json` — legacy, still used on Windows + many macOS installs
//!   3. `~/.config/claude/.credentials.json` — Claude Code v1.0.30+ on some platforms
//!
//! Loads are read-only. `write_back` is the ONLY writer of this file and uses
//! atomic tmp+rename, preserves the original's permissions, reuses Anthropic's
//! file (never invents a new one), and touches only the OAuth token fields
//! (AGENTS.md §3.4). No other crate reads or writes these credentials.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::types::{Credentials, OAuthError, RefreshedTokens};

/// Return the first existing credentials path, or `CredentialsMissing` listing
/// every path searched.
pub fn locate_credentials() -> Result<PathBuf, OAuthError> {
    let candidates = candidate_paths();
    for path in &candidates {
        if path.exists() {
            return Ok(path.clone());
        }
    }
    Err(OAuthError::CredentialsMissing {
        searched: candidates,
    })
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(xdg).join("claude").join(".credentials.json"));
    }
    if let Some(home) = home_dir() {
        out.push(home.join(".claude").join(".credentials.json"));
        out.push(
            home.join(".config")
                .join("claude")
                .join(".credentials.json"),
        );
    }
    out
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Load credentials from a specific path. Useful for tests and for explicit
/// override paths.
pub fn load_from(path: &Path) -> Result<Credentials, OAuthError> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => OAuthError::CredentialsMissing {
            searched: vec![path.to_path_buf()],
        },
        _ => OAuthError::Io {
            path: path.to_path_buf(),
            source: e,
        },
    })?;
    let creds: Credentials =
        serde_json::from_slice(&bytes).map_err(|e| OAuthError::CredentialsMalformed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    Ok(creds)
}

/// Locate credentials in the standard search paths and load them.
pub fn load() -> Result<Credentials, OAuthError> {
    let path = locate_credentials()?;
    load_from(&path)
}

/// Outcome of [`write_back`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteBack {
    /// The file was atomically replaced with the refreshed tokens.
    Written,
    /// The on-disk token's `expiresAt` was already >= ours (Claude Code or
    /// another Balanze refreshed concurrently). File left untouched; caller
    /// should keep using whatever token it just minted in memory.
    SkippedDiskNewer,
}

/// Atomically write refreshed OAuth tokens back into the existing credentials
/// file. AGENTS.md §3.4: tmp+rename in the same dir, preserve the original's
/// permissions, reuse Anthropic's file (never invent a new one), touch only
/// the three token fields, never regress a concurrently-newer on-disk token.
///
/// Note: the file is rewritten via `serde_json::to_vec_pretty`, so it is
/// normalized to pretty-printed JSON with object keys in sorted order
/// (`serde_json::Value` is a `BTreeMap`; the workspace does not enable the
/// `preserve_order` feature). This is semantically safe — Claude Code and
/// Balanze both re-parse by key — but the rewritten file is intentionally
/// not byte-identical to Claude Code's original compact layout.
///
/// TODO(v0.2): the read→refresh→write race with Claude Code's own refresh is
/// only "skip if disk newer" here. A long-running watcher must serialize
/// refreshes and re-read on `SkippedDiskNewer`; for the one-shot CLI the race
/// window is ~1s and benign.
pub fn write_back(path: &Path, refreshed: &RefreshedTokens) -> Result<WriteBack, OAuthError> {
    // Re-read fresh — Claude Code may have rewritten the file since we loaded.
    // Parse as generic JSON so every unknown key round-trips untouched.
    let bytes = std::fs::read(path).map_err(|e| OAuthError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut root: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| OAuthError::CredentialsMalformed {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

    let oauth = root
        .get_mut("claudeAiOauth")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| OAuthError::CredentialsMalformed {
            path: path.to_path_buf(),
            reason: "missing claudeAiOauth object".to_string(),
        })?;

    let disk_expires = oauth.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);
    if disk_expires >= refreshed.expires_at_ms {
        return Ok(WriteBack::SkippedDiskNewer);
    }

    oauth.insert(
        "accessToken".into(),
        serde_json::json!(refreshed.access_token),
    );
    oauth.insert(
        "refreshToken".into(),
        serde_json::json!(refreshed.refresh_token),
    );
    oauth.insert(
        "expiresAt".into(),
        serde_json::json!(refreshed.expires_at_ms),
    );

    let serialized =
        serde_json::to_vec_pretty(&root).map_err(|e| OAuthError::CredentialsMalformed {
            path: path.to_path_buf(),
            reason: format!("re-serialize: {e}"),
        })?;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    // Fix 2: unique name = pid + nanosecond timestamp + monotonic counter,
    // avoiding collisions on the 401-retry double-refresh path and on PID reuse.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let tmp = dir.join(format!(
        ".credentials.json.balanze-{}-{}-{}.tmp",
        std::process::id(),
        nanos,
        seq,
    ));

    // Fix 3 (unix): create the tmp file with mode 0o600 from the start so
    // there is never a world/group-readable window for this secret file.
    // Windows: the file inherits the parent directory's ACL (user profile),
    // which is the documented, acceptable Windows behavior (no mode concept;
    // same-dir inheritance preserves access scope).
    let write_result = (|| -> std::io::Result<()> {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create_new(&tmp)?;

        f.write_all(&serialized)?;
        // Fix 1: fsync the tmp before rename so a crash/power-loss between
        // write and rename cannot lose the refreshed credentials.
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        // Fix 2: clean up tmp on write failure too (not only on rename failure).
        let _ = std::fs::remove_file(&tmp);
        return Err(OAuthError::Io {
            path: tmp,
            source: e,
        });
    }

    // Fix 3 (unix): copy the original file's permissions onto the tmp.
    // The tmp is already 0o600 (safe), so a copy failure is safe to ignore
    // (original is typically 0o600 anyway; we simply keep our restrictive default).
    #[cfg(unix)]
    {
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        OAuthError::Io {
            path: path.to_path_buf(),
            source: e,
        }
    })?;

    // Fix 1: fsync the parent directory on Unix so the rename itself is
    // durable. Best-effort — a dir-fsync failure must not fail the write
    // since the data is already renamed into place.
    // On Windows, opening a directory as a File for sync is not supported;
    // the file fsync + rename is the portable durability guarantee.
    #[cfg(unix)]
    {
        let _ = std::fs::File::open(dir).and_then(|f| f.sync_all());
    }

    Ok(WriteBack::Written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_valid_credentials_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".credentials.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            br#"{
                "claudeAiOauth": {
                    "accessToken": "sk-ant-oat01-test",
                    "refreshToken": "sk-ant-ort01-test",
                    "expiresAt": 1778667589158,
                    "subscriptionType": "max",
                    "rateLimitTier": "default_claude_max_5x",
                    "scopes": ["user:profile", "user:sessions:claude_code"]
                }
            }"#,
        )
        .unwrap();
        let creds = load_from(&path).expect("load");
        assert_eq!(creds.claude_ai_oauth.access_token, "sk-ant-oat01-test");
        assert_eq!(
            creds.claude_ai_oauth.subscription_type.as_deref(),
            Some("max")
        );
        assert_eq!(creds.claude_ai_oauth.expires_at, 1778667589158);
        assert_eq!(creds.claude_ai_oauth.scopes.len(), 2);
    }

    #[test]
    fn loads_minimal_credentials_file() {
        // Only required field is accessToken + expiresAt; everything else optional.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(
            &path,
            br#"{"claudeAiOauth":{"accessToken":"x","expiresAt":0}}"#,
        )
        .unwrap();
        let creds = load_from(&path).expect("load minimal");
        assert_eq!(creds.claude_ai_oauth.access_token, "x");
        assert!(creds.claude_ai_oauth.refresh_token.is_none());
        assert!(creds.claude_ai_oauth.scopes.is_empty());
    }

    #[test]
    fn missing_file_returns_credentials_missing() {
        let path = std::env::temp_dir().join("balanze-test-missing-xyzzy.json");
        let _ = std::fs::remove_file(&path);
        match load_from(&path) {
            Err(OAuthError::CredentialsMissing { searched }) => {
                assert_eq!(searched, vec![path]);
            }
            other => panic!("expected CredentialsMissing, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_returns_credentials_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        match load_from(&path) {
            Err(OAuthError::CredentialsMalformed { path: p, reason }) => {
                assert_eq!(p, path);
                assert!(!reason.is_empty());
            }
            other => panic!("expected CredentialsMalformed, got {other:?}"),
        }
    }

    #[test]
    fn missing_claude_ai_oauth_key_returns_credentials_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        // Valid JSON, but missing the claudeAiOauth key.
        std::fs::write(&path, br#"{"otherStuff": {}}"#).unwrap();
        match load_from(&path) {
            Err(OAuthError::CredentialsMalformed { .. }) => {}
            other => panic!("expected CredentialsMalformed, got {other:?}"),
        }
    }

    use crate::types::RefreshedTokens;

    fn write_creds(path: &Path, access: &str, refresh: &str, expires: i64, extra: &str) {
        std::fs::write(
            path,
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{expires},"subscriptionType":"max","scopes":["user:profile"]}},"otherTool":{extra}}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn write_back_updates_tokens_and_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        write_creds(&path, "old-acc", "old-ref", 100, r#"{"keep":true}"#);

        let r = RefreshedTokens {
            access_token: "new-acc".into(),
            refresh_token: "new-ref".into(),
            expires_at_ms: 999,
        };
        assert!(matches!(write_back(&path, &r).unwrap(), WriteBack::Written));

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["claudeAiOauth"]["accessToken"], "new-acc");
        assert_eq!(v["claudeAiOauth"]["refreshToken"], "new-ref");
        assert_eq!(v["claudeAiOauth"]["expiresAt"], 999);
        assert_eq!(v["claudeAiOauth"]["subscriptionType"], "max");
        assert_eq!(v["otherTool"]["keep"], true);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp not cleaned");
    }

    #[test]
    fn write_back_skips_when_disk_is_already_newer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        write_creds(&path, "disk-acc", "disk-ref", 5000, "null");

        let r = RefreshedTokens {
            access_token: "ours".into(),
            refresh_token: "ours".into(),
            expires_at_ms: 4000,
        };
        assert!(matches!(
            write_back(&path, &r).unwrap(),
            WriteBack::SkippedDiskNewer
        ));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["claudeAiOauth"]["accessToken"], "disk-acc");
    }

    #[cfg(unix)]
    #[test]
    fn write_back_preserves_unix_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        write_creds(&path, "a", "b", 1, "null");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let r = RefreshedTokens {
            access_token: "a2".into(),
            refresh_token: "b2".into(),
            expires_at_ms: 2,
        };
        write_back(&path, &r).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode not preserved");
    }

    #[test]
    fn write_back_malformed_existing_file_is_credentials_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let r = RefreshedTokens {
            access_token: "a".into(),
            refresh_token: "b".into(),
            expires_at_ms: 1,
        };
        match write_back(&path, &r) {
            Err(OAuthError::CredentialsMalformed { .. }) => {}
            other => panic!("expected CredentialsMalformed, got {other:?}"),
        }
    }

    #[test]
    fn write_back_missing_claude_ai_oauth_is_credentials_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, br#"{"somethingElse": {}}"#).unwrap();
        let r = RefreshedTokens {
            access_token: "a".into(),
            refresh_token: "b".into(),
            expires_at_ms: 1,
        };
        match write_back(&path, &r) {
            Err(OAuthError::CredentialsMalformed { .. }) => {}
            other => panic!("expected CredentialsMalformed, got {other:?}"),
        }
    }
}
