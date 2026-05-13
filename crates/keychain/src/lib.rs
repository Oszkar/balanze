//! Cross-OS keychain wrapper. The only crate in the workspace that imports
//! the `keyring` crate directly — see AGENTS.md §4 #5.
//!
//! All user-supplied API keys (OpenAI, and future providers) flow through
//! this API. The service name is fixed at `"me.oszkar.Balanze"`; entry names
//! are namespaced constants in `keys`.
//!
//! On macOS: macOS Keychain. On Windows: Credential Manager. (Linux: Secret
//! Service via libsecret — out of v0.1 scope but `keyring` supports it.)
//!
//! Reads and writes can prompt the user for permission on first use; the
//! caller should not assume operations are silent.

use thiserror::Error;
use tracing::debug;

const SERVICE: &str = "me.oszkar.Balanze";

/// Stable entry names for keychain items. Adding a new secret means adding
/// a constant here so the call sites are all greppable.
pub mod keys {
    /// User-supplied OpenAI Platform API key (`sk-…`). One per Balanze install.
    pub const OPENAI_API_KEY: &str = "openai_api_key";
}

#[derive(Debug, Error)]
pub enum KeychainError {
    #[error("no entry found for `{0}`")]
    NotFound(String),

    #[error("OS keychain access failed for `{name}`: {reason}")]
    PlatformError { name: String, reason: String },
}

/// Store a value under the given entry name. Overwrites any existing value.
pub fn set(name: &str, value: &str) -> Result<(), KeychainError> {
    debug!(name, "keychain: set");
    let entry = keyring::Entry::new(SERVICE, name).map_err(|e| KeychainError::PlatformError {
        name: name.to_string(),
        reason: e.to_string(),
    })?;
    entry
        .set_password(value)
        .map_err(|e| KeychainError::PlatformError {
            name: name.to_string(),
            reason: e.to_string(),
        })
}

/// Read the value stored under the given entry name.
///
/// Returns `NotFound` if there's no entry; `PlatformError` for any other
/// failure (locked keychain, permission denied, etc.).
pub fn get(name: &str) -> Result<String, KeychainError> {
    debug!(name, "keychain: get");
    let entry = keyring::Entry::new(SERVICE, name).map_err(|e| KeychainError::PlatformError {
        name: name.to_string(),
        reason: e.to_string(),
    })?;
    match entry.get_password() {
        Ok(v) => Ok(v),
        Err(keyring::Error::NoEntry) => Err(KeychainError::NotFound(name.to_string())),
        Err(e) => Err(KeychainError::PlatformError {
            name: name.to_string(),
            reason: e.to_string(),
        }),
    }
}

/// Delete the entry under the given name. Returns `Ok(())` if it didn't
/// exist (delete is idempotent).
pub fn delete(name: &str) -> Result<(), KeychainError> {
    debug!(name, "keychain: delete");
    let entry = keyring::Entry::new(SERVICE, name).map_err(|e| KeychainError::PlatformError {
        name: name.to_string(),
        reason: e.to_string(),
    })?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeychainError::PlatformError {
            name: name.to_string(),
            reason: e.to_string(),
        }),
    }
}

/// Check whether an entry exists for the given name.
///
/// Implemented as `get(...)` and discarding the value. There's no cheaper
/// "exists" primitive in the `keyring` crate; this can prompt the user for
/// access on macOS exactly like a real read would.
pub fn exists(name: &str) -> Result<bool, KeychainError> {
    match get(name) {
        Ok(_) => Ok(true),
        Err(KeychainError::NotFound(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test against the real OS keychain. Ignored by default because
    /// CI runners don't have a usable keychain on every platform; run
    /// manually on each dev machine before tagging a release.
    ///
    /// **Known issue (May 2026)**: This test fails on Windows with
    /// `keyring = "3.6.3"` — set returns Ok but a subsequent get returns
    /// NoEntry, meaning the credential never actually persists. The fix is
    /// to migrate to `keyring-core` (the v4 successor crate) with an
    /// explicit `set_default_store` initialization. Tracked as a v0.2 task;
    /// in the meantime, the CLI honors a `BALANZE_OPENAI_KEY` env var as a
    /// fallback. See AGENTS.md "Known issues" section.
    #[test]
    #[ignore]
    fn roundtrip_against_real_keychain() {
        let test_name = "balanze_test_roundtrip_xyzzy";
        let _ = delete(test_name);
        assert!(!exists(test_name).unwrap(), "test entry pre-existed");

        set(test_name, "test-value-123").expect("set");
        assert!(exists(test_name).unwrap());
        assert_eq!(get(test_name).unwrap(), "test-value-123");

        // Overwrite with a different value.
        set(test_name, "test-value-456").expect("overwrite");
        assert_eq!(get(test_name).unwrap(), "test-value-456");

        delete(test_name).expect("delete");
        assert!(!exists(test_name).unwrap());
        // Delete is idempotent.
        delete(test_name).expect("delete-again");

        match get(test_name) {
            Err(KeychainError::NotFound(name)) => assert_eq!(name, test_name),
            other => panic!("expected NotFound after delete, got {other:?}"),
        }
    }

    #[test]
    fn keys_module_exposes_well_known_constants() {
        assert_eq!(keys::OPENAI_API_KEY, "openai_api_key");
    }
}
