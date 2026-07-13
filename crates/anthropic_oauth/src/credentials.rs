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
//! Every source is read-only (AGENTS.md §3.4). Claude Code owns both the file
//! and Keychain forms, so Balanze never refreshes, rewrites, mirrors, or backs
//! up either credential. No other crate reads these credentials.

use std::path::{Path, PathBuf};

use crate::types::{Credentials, OAuthError};

/// macOS login-Keychain generic-password service that recent Claude Code
/// writes its OAuth credential under. The stored value is the same JSON shape
/// (`{"claudeAiOauth": {...}}`) the file held.
#[cfg(target_os = "macos")]
const MACOS_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Where Balanze found Claude Code's read-only OAuth credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// A credentials file owned and updated by Claude Code.
    File(PathBuf),
    /// The macOS login Keychain entry owned and updated by Claude Code.
    #[cfg(target_os = "macos")]
    MacosKeychain,
}

impl CredentialSource {
    /// Whether repeated polls should reuse the loaded credential. File sources
    /// are cheap to re-read and must observe Claude Code's atomic replacements;
    /// the macOS Keychain source is cached to avoid repeated access prompts.
    pub fn cache_between_polls(&self) -> bool {
        match self {
            CredentialSource::File(_) => false,
            #[cfg(target_os = "macos")]
            CredentialSource::MacosKeychain => true,
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

/// Locate the credential source. Prefers a file; on macOS, falls back to the
/// login Keychain when no file exists. Returns
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
        Ok(CredentialSource::MacosKeychain)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn file_source_is_read_only_and_not_cached() {
        let src = CredentialSource::File(PathBuf::from("/tmp/.credentials.json"));
        assert!(!src.cache_between_polls());
        assert!(src.describe().contains(".credentials.json"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keychain_source_is_read_only() {
        let src = CredentialSource::MacosKeychain;
        assert!(src.cache_between_polls());
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

    fn write_creds(path: &Path, access: &str, expires: i64) {
        std::fs::write(
            path,
            format!(r#"{{"claudeAiOauth":{{"accessToken":"{access}","expiresAt":{expires}}}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn external_atomic_replacement_is_observed_without_balanze_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        write_creds(&path, "old", 100);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            load_from(&path).unwrap().claude_ai_oauth.access_token,
            "old"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "load must not rewrite"
        );

        let replacement = dir.path().join("replacement.json");
        write_creds(&replacement, "new", 200);
        std::fs::rename(&replacement, &path).unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.claude_ai_oauth.access_token, "new");
        assert_eq!(loaded.claude_ai_oauth.expires_at, 200);
    }
}
