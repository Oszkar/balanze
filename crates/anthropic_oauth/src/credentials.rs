//! Locate and load Claude Code's credentials file.
//!
//! Search order (first existing path wins):
//!   1. `$XDG_CONFIG_HOME/claude/.credentials.json` (if XDG_CONFIG_HOME is set)
//!   2. `~/.claude/.credentials.json` — legacy, still used on Windows + many macOS installs
//!   3. `~/.config/claude/.credentials.json` — Claude Code v1.0.30+ on some platforms
//!
//! Loads are READ-ONLY. The crate never writes to this file in v0.1. When the
//! refresh-token flow lands in step 4, writes will use atomic tmp+rename.

use std::path::{Path, PathBuf};

use crate::types::{Credentials, OAuthError};

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
        out.push(home.join(".config").join("claude").join(".credentials.json"));
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
        assert_eq!(creds.claude_ai_oauth.subscription_type.as_deref(), Some("max"));
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
}
