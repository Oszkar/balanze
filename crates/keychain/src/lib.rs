//! Cross-OS keychain wrapper. The only crate in the workspace that imports
//! `keyring-core` and the native store crates directly - see AGENTS.md §4 #5.
//!
//! All user-supplied API keys (OpenAI, and future providers) flow through
//! this API. The service name is fixed at `"me.oszkar.Balanze"`; entry names
//! are namespaced constants in `keys`.
//!
//! On macOS: macOS Keychain. On Windows: Credential Manager. On every other
//! platform (Linux) no native store is wired: [`get`], [`set`], and [`delete`]
//! return [`KeychainError::NoStore`], and callers route the user to
//! [`NO_STORE_HINT`]. [`resolve_openai_key`] is the deliberate exception - its
//! contract is a configured-or-not question, so a storeless platform resolves
//! to `Ok(None)` rather than an error.
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
/// keychain ops there return [`KeychainError::NoStore`].
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

    /// This platform has no native credential store wired (see
    /// [`init_default_store`]). Distinct from [`KeychainError::PlatformError`]
    /// because it is an expected, documented platform condition rather than a
    /// fault: callers should route the user to [`NO_STORE_HINT`] instead of
    /// reporting a broken keychain.
    #[error("no OS credential store is available on this platform")]
    NoStore,
}

/// Guidance for the [`KeychainError::NoStore`] case. Kept here so every surface
/// that hits a storeless platform tells the user the same thing.
pub const NO_STORE_HINT: &str = "Set the BALANZE_OPENAI_KEY environment variable instead; it takes precedence over the keychain.";

/// Whether this platform has a native credential store wired.
///
/// Mirrors the `cfg` on the store registration in [`init_default_store`]. Adding
/// a backend for a new platform means updating both.
pub const fn has_native_store() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// Store a value under the given entry name. Overwrites any existing value.
pub fn set(name: &str, value: &str) -> Result<(), KeychainError> {
    if !has_native_store() {
        return Err(KeychainError::NoStore);
    }
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
    if !has_native_store() {
        return Err(KeychainError::NoStore);
    }
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

/// Resolve the user's OpenAI admin key, honoring the documented
/// `BALANZE_OPENAI_KEY` env override (trimmed; empty = unset), which takes
/// precedence over the stored keychain entry (AGENTS.md §3.4).
///
/// `Ok(None)` means "not configured", including on a platform with no credential
/// store at all. `Err` is a real keychain failure (locked, denied), not mere
/// absence. Single source of truth for the CLI snapshot fetch,
/// the statusline self-compose fingerprint, and the watcher poll task.
///
/// The env var, **when present, is authoritative even if blank**: a blank or
/// whitespace `BALANZE_OPENAI_KEY` explicitly means "not configured" and does
/// NOT fall through to the keychain. Callers set it blank to force OpenAI off /
/// no network (see `balanze_cli` statusline self-compose); reading the keychain
/// there could fetch or prompt on a machine with a saved key. The keychain is
/// consulted only when the env var is entirely absent.
pub fn resolve_openai_key() -> Result<Option<String>, KeychainError> {
    resolve_openai_key_with(std::env::var("BALANZE_OPENAI_KEY").ok())
}

/// Inner logic of [`resolve_openai_key`] with the env value injected (as
/// `std::env::var(...).ok()`, so `None` = absent, `Some(_)` = present). Keeps
/// the present/absent branch and the trim rule unit-testable without a store.
fn resolve_openai_key_with(env_var: Option<String>) -> Result<Option<String>, KeychainError> {
    // Present (blank or not) is authoritative; a blank value is `None` here and
    // must not fall through to the keychain below.
    if let Some(raw) = env_var {
        return Ok(non_blank(&raw));
    }
    match get(keys::OPENAI_API_KEY) {
        Ok(k) => Ok(Some(k)),
        Err(KeychainError::NotFound(_)) => Ok(None),
        // No store means nothing can ever have been stored, so "not configured"
        // is literally true. The typed error stays on get/set/delete, where the
        // CLI needs it to tell the user where to put the key instead.
        Err(KeychainError::NoStore) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Trim a raw value; `None` if it is empty or all whitespace. Pure.
fn non_blank(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Delete the entry under the given name. Returns `Ok(())` if it didn't
/// exist (delete is idempotent).
pub fn delete(name: &str) -> Result<(), KeychainError> {
    if !has_native_store() {
        return Err(KeychainError::NoStore);
    }
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

    #[test]
    fn present_nonblank_env_wins_and_is_trimmed() {
        assert_eq!(
            resolve_openai_key_with(Some("  sk-admin-xyz  ".to_string())).unwrap(),
            Some("sk-admin-xyz".to_string())
        );
    }

    #[test]
    fn present_but_blank_env_is_not_configured_without_touching_keychain() {
        // Regression guard (PR #165 review): a present-but-blank BALANZE_OPENAI_KEY
        // must resolve to Ok(None) directly and must NOT fall through to the
        // keychain - callers set it blank to force "OpenAI off / no network", and
        // a machine with a saved key (or a prompting keychain) would otherwise
        // fetch or prompt. Testable without a store precisely because the
        // env-present branch never calls `get`.
        for blank in ["", "   ", "\t\n"] {
            assert_eq!(
                resolve_openai_key_with(Some(blank.to_string())).unwrap(),
                None,
                "blank env {blank:?} must be Ok(None)"
            );
        }
    }

    #[test]
    fn has_native_store_matches_the_compiled_target() {
        // Windows and macOS wire a store in `init_default_store`; nothing else
        // does. This mirrors the cfg on the store registration, so a future
        // Linux backend must update both together.
        let expected = cfg!(any(windows, target_os = "macos"));
        assert_eq!(has_native_store(), expected);
    }

    #[test]
    fn no_store_error_names_the_platform_gap_without_keyring_jargon() {
        let msg = KeychainError::NoStore.to_string();
        assert_eq!(msg, "no OS credential store is available on this platform");
        // The whole point of the variant is that the user never sees raw
        // keyring-core text for an expected platform condition.
        assert!(!msg.contains("keyring"));
    }

    #[test]
    fn no_store_hint_points_at_the_documented_env_override() {
        assert!(NO_STORE_HINT.contains("BALANZE_OPENAI_KEY"));
    }

    /// On a platform with no store, every operation reports `NoStore` rather
    /// than a `PlatformError` wrapping keyring-core's "no default store" text.
    /// Only compiled where that is the real behavior.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn storeless_platform_returns_no_store_from_every_operation() {
        init_default_store();
        assert!(matches!(get("any_name"), Err(KeychainError::NoStore)));
        assert!(matches!(set("any_name", "v"), Err(KeychainError::NoStore)));
        assert!(matches!(delete("any_name"), Err(KeychainError::NoStore)));
    }

    /// `resolve_openai_key`'s contract is a configured/not question, so a
    /// storeless platform means "not configured" - literally true, since
    /// nothing can ever have been stored. Returning Err here would make
    /// `sources.rs`'s `live_fetch_openai` report an error for an unconfigured
    /// Linux user (its doc promises Err only for real fetch failures) and would
    /// make the watcher's openai_poll log a warning on every launch for a
    /// permanent platform condition (AGENTS.md §3.2 reserves warn for things
    /// worth noticing).
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn storeless_platform_resolves_the_key_as_not_configured() {
        init_default_store();
        // Env var absent, so this falls through to the keychain.
        assert!(matches!(resolve_openai_key_with(None), Ok(None)));
    }
}
