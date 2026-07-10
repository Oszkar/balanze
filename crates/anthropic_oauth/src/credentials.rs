//! Locate and load Claude Code's OAuth credentials.
//!
//! A credential lives in one of two places, modelled by [`CredentialSource`]:
//!
//! - **A file** (the first existing path wins):
//!   1. `$XDG_CONFIG_HOME/claude/.credentials.json` (if XDG_CONFIG_HOME is set)
//!   2. `~/.claude/.credentials.json` - legacy, still used on Windows + many macOS installs
//!   3. `~/.config/claude/.credentials.json` - Claude Code v1.0.30+ on some platforms
//! - **The macOS login Keychain** (generic password, service
//!   `"Claude Code-credentials"`) - recent Claude Code on macOS stores the
//!   credential here instead of a file. Used only when no file exists.
//!
//! Refresh policy follows ownership (AGENTS.md §3.4): Balanze owns the file it
//! refreshes, so a file source may be refreshed and written back via
//! [`write_back`] (the ONLY writer - atomic tmp+rename, perms-preserving,
//! touches only the OAuth token fields). The Keychain entry is Claude Code's;
//! the Keychain source is **read-only** - Balanze never refreshes or writes a
//! credential it does not own. No other crate reads or writes these credentials.

use std::path::{Path, PathBuf};

use crate::types::{Credentials, OAuthError, RefreshedTokens};

/// macOS login-Keychain generic-password service that recent Claude Code
/// writes its OAuth credential under. The stored value is the same JSON shape
/// (`{"claudeAiOauth": {...}}`) the file held.
#[cfg(target_os = "macos")]
const MACOS_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Where Balanze found Claude Code's OAuth credential. Determines whether
/// Balanze may refresh + write back the token (it owns the file) or must treat
/// it as read-only (the macOS login Keychain entry belongs to Claude Code -
/// AGENTS.md §3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// A credentials file Balanze may refresh and atomically write back.
    File(PathBuf),
    /// The macOS login Keychain entry owned by Claude Code. Read-only: Balanze
    /// uses the token while valid and never refreshes or writes it back.
    #[cfg(target_os = "macos")]
    MacosKeychain,
}

impl CredentialSource {
    /// The file path Balanze may write refreshed tokens back to, or `None` for
    /// a read-only source (the macOS Keychain entry Claude Code owns). Callers
    /// gate the refresh/write-back path on this.
    pub fn writable_path(&self) -> Option<&Path> {
        match self {
            CredentialSource::File(p) => Some(p.as_path()),
            #[cfg(target_os = "macos")]
            CredentialSource::MacosKeychain => None,
        }
    }

    /// A human-readable description for setup/diagnostic output. Never includes
    /// any credential material - only the location.
    pub fn describe(&self) -> String {
        match self {
            CredentialSource::File(p) => p.display().to_string(),
            #[cfg(target_os = "macos")]
            CredentialSource::MacosKeychain => {
                format!("macOS login Keychain (service \"{MACOS_KEYCHAIN_SERVICE}\")")
            }
        }
    }
}

/// Locate the credential source. Prefers a file (so Balanze can refresh it);
/// on macOS, falls back to the login Keychain when no file exists. Returns
/// `CredentialsMissing` (listing the file paths searched) on platforms without
/// a Keychain fallback when nothing is found.
///
/// The Keychain source is returned optimistically without reading here;
/// [`load_from_source`] performs the single read (which may prompt for Keychain
/// access) and maps a missing entry to `CredentialsMissing`, so callers degrade
/// exactly as they would for an absent file.
pub fn locate_credentials() -> Result<CredentialSource, OAuthError> {
    let candidates = candidate_paths();
    for path in &candidates {
        if path.exists() {
            return Ok(CredentialSource::File(path.clone()));
        }
    }
    #[cfg(target_os = "macos")]
    {
        return Ok(CredentialSource::MacosKeychain);
    }
    #[cfg(not(target_os = "macos"))]
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

/// Load credentials from a located source. For a file source this is
/// [`load_from`]; for the macOS Keychain source it reads the entry.
pub fn load_from_source(source: &CredentialSource) -> Result<Credentials, OAuthError> {
    match source {
        CredentialSource::File(p) => load_from(p),
        #[cfg(target_os = "macos")]
        CredentialSource::MacosKeychain => load_from_macos_keychain(),
    }
}

/// Read Claude Code's OAuth credential from the macOS login Keychain.
///
/// Shells out to `/usr/bin/security` rather than taking a crate dependency:
/// keeps `anthropic_oauth` self-contained (the `keychain` crate is the only one
/// that imports the keyring stack - AGENTS.md §4 #5) and adds no new dep. The
/// read is best-effort and may prompt the user for Keychain access on first
/// use, exactly like any other process reading another app's entry.
///
/// A missing entry maps to `CredentialsMissing` so callers degrade like an
/// absent file (e.g. the watcher's clean startup exit when Claude Code isn't
/// installed). Any other failure surfaces as a real error. `security`'s stderr
/// is a diagnostic, not the secret value, so it is safe to include in errors.
#[cfg(target_os = "macos")]
fn load_from_macos_keychain() -> Result<Credentials, OAuthError> {
    use std::process::Command;

    // Pseudo-path used only for error context; the type is `PathBuf`.
    let marker = PathBuf::from(format!(
        "macOS login Keychain (service \"{MACOS_KEYCHAIN_SERVICE}\")"
    ));

    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", MACOS_KEYCHAIN_SERVICE, "-w"])
        .output()
        .map_err(|e| OAuthError::Io {
            path: marker.clone(),
            source: e,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // `security` exits 44 (errSecItemNotFound) when the entry is absent.
        let not_found = output.status.code() == Some(44) || stderr.contains("could not be found");
        if not_found {
            return Err(OAuthError::CredentialsMissing {
                searched: vec![marker],
            });
        }
        return Err(OAuthError::CredentialsMalformed {
            path: marker,
            reason: format!("`security` failed: {}", stderr.trim()),
        });
    }

    // `-w` prints only the password (the JSON), with a trailing newline.
    let raw = String::from_utf8(output.stdout).map_err(|e| OAuthError::CredentialsMalformed {
        path: marker.clone(),
        reason: format!("keychain value is not valid UTF-8: {e}"),
    })?;
    serde_json::from_str(raw.trim()).map_err(|e| OAuthError::CredentialsMalformed {
        path: marker,
        reason: e.to_string(),
    })
}

/// Locate the credential source and load it.
pub fn load() -> Result<Credentials, OAuthError> {
    load_from_source(&locate_credentials()?)
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
/// `preserve_order` feature). This is semantically safe - Claude Code and
/// Balanze both re-parse by key - but the rewritten file is intentionally
/// not byte-identical to Claude Code's original compact layout.
///
/// TODO: the read→refresh→write race with Claude Code's own refresh is
/// only "skip if disk newer" here. A long-running watcher must serialize
/// refreshes and re-read on `SkippedDiskNewer`; for the one-shot CLI the race
/// window is ~1s and benign.
pub fn write_back(path: &Path, refreshed: &RefreshedTokens) -> Result<WriteBack, OAuthError> {
    // Re-read fresh - Claude Code may have rewritten the file since we loaded.
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

    // Create the tmp 0o600 on unix so there is never a world/group-readable
    // window for this secret file (Windows inherits the parent dir ACL). This
    // crate still owns the merge above - touch only the `claudeAiOauth` token
    // fields, never regress a concurrently-newer on-disk token; `atomic_file`
    // does only the byte-level durable replace.
    atomic_file::atomic_write(path, &serialized, atomic_file::Permissions::OwnerOnly)
        .map(|()| WriteBack::Written)
        .map_err(|source| OAuthError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn file_source_is_writable() {
        let src = CredentialSource::File(PathBuf::from("/tmp/.credentials.json"));
        assert_eq!(
            src.writable_path(),
            Some(Path::new("/tmp/.credentials.json"))
        );
        assert!(src.describe().contains(".credentials.json"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keychain_source_is_read_only() {
        let src = CredentialSource::MacosKeychain;
        // Read-only: no path to write a refreshed token back to. This is the
        // gate the CLI + watcher use to skip refresh for a credential we don't
        // own (AGENTS.md §3.4).
        assert_eq!(src.writable_path(), None);
        assert!(src.describe().contains("Claude Code-credentials"));
    }

    /// Smoke test against the real macOS login Keychain. Ignored by default
    /// (needs Claude Code's credential present); run manually on macOS per
    /// AGENTS.md §6 before tagging a release.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn reads_real_macos_keychain_credential() {
        let creds = load_from_macos_keychain().expect("read Claude Code keychain credential");
        assert!(
            !creds.claude_ai_oauth.access_token.is_empty(),
            "access token should be non-empty"
        );
    }

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
