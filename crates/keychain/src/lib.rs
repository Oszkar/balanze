//! Cross-OS keychain wrapper. The only crate in the workspace that imports
//! `keyring-core` and the native store crates directly - see AGENTS.md §4 #5.
//!
//! All user-supplied API keys (OpenAI, and future providers) flow through
//! this API. The service name is fixed at `"me.oszkar.Balanze"`; entry names
//! are namespaced constants in `keys`.
//!
//! On macOS: macOS Keychain. On Windows: Credential Manager. (Linux: no native
//! store is wired - out of scope; keychain ops fail there until one is added.)
//!
//! keyring-core has no default credential store until one is registered, so
//! each binary MUST call [`init_default_store`] once at startup before any
//! get/set/delete. The store crates are pulled in per-target by this crate's
//! `Cargo.toml`.
//!
//! Reads and writes can prompt the user for permission on first use; the
//! caller should not assume operations are silent.

use thiserror::Error;
use tracing::debug;

const SERVICE: &str = "me.oszkar.Balanze";

/// Register the platform-native credential store as keyring-core's default.
///
/// Must be called once at process startup, before any [`get`]/[`set`]/[`delete`].
/// Idempotent: subsequent calls are no-ops (guarded by [`std::sync::Once`]), so
/// it is safe to call defensively. On platforms without a native store wired
/// (e.g. Linux), this logs a warning and leaves keyring-core storeless, so
/// keychain ops there return [`KeychainError::PlatformError`].
pub fn init_default_store() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        #[cfg(windows)]
        match windows_native_keyring_store::Store::new() {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => tracing::error!("keychain: failed to init Windows credential store: {e}"),
        }
        #[cfg(target_os = "macos")]
        match apple_native_keyring_store::keychain::Store::new() {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => tracing::error!("keychain: failed to init macOS keychain store: {e}"),
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        tracing::warn!(
            "keychain: no native credential store for this platform; keychain operations will fail"
        );
    });
}

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
    let entry =
        keyring_core::Entry::new(SERVICE, name).map_err(|e| KeychainError::PlatformError {
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
    let entry =
        keyring_core::Entry::new(SERVICE, name).map_err(|e| KeychainError::PlatformError {
            name: name.to_string(),
            reason: e.to_string(),
        })?;
    match entry.get_password() {
        Ok(v) => Ok(v),
        Err(keyring_core::Error::NoEntry) => Err(KeychainError::NotFound(name.to_string())),
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
    let entry =
        keyring_core::Entry::new(SERVICE, name).map_err(|e| KeychainError::PlatformError {
            name: name.to_string(),
            reason: e.to_string(),
        })?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeychainError::PlatformError {
            name: name.to_string(),
            reason: e.to_string(),
        }),
    }
}

/// Check whether an entry exists for the given name.
///
/// Implemented as `get(...)` and discarding the value. There's no cheaper
/// "exists" primitive in keyring-core; this can prompt the user for
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
    /// manually on each dev machine before tagging a release (per AGENTS.md §6).
    ///
    /// Regression guard for real keychain roundtrip behavior - set, overwrite,
    /// get, and delete must persist across fresh entry handles. A backend that
    /// silently drops writes would pass a same-handle check but fail here.
    #[test]
    #[ignore]
    fn roundtrip_against_real_keychain() {
        init_default_store();
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
