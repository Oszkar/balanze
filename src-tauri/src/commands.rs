//! Tauri command surface (IPC contract, AGENTS.md §4 #9).
//!
//! Read/refresh: `get_snapshot` + `refresh_now` reach the coordinator via the
//! managed `StateCoordinatorHandle` (async - they await the coordinator).
//!
//! Settings/keys: `get_settings` / `set_settings` / `set_api_key` are sync
//! commands. Tauri runs non-async commands on a dedicated thread (not the
//! async runtime), so their blocking keychain + `settings.json` I/O does not
//! stall a tokio worker (AGENTS.md §2.1). Secret hygiene (§3.4): the API key
//! is never logged or echoed, and `get_settings` returns only the non-secret
//! `Settings` shape - the key itself never crosses back to the frontend.
//!
//! All commands return `Result<_, String>` derived from `anyhow`/error
//! `to_string()`.

use settings::Settings;
use state_coordinator::{Snapshot, StateCoordinatorHandle, StateMsg};
use tauri::State;

#[tauri::command]
pub async fn get_snapshot(handle: State<'_, StateCoordinatorHandle>) -> Result<Snapshot, String> {
    handle.query().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn refresh_now(handle: State<'_, StateCoordinatorHandle>) -> Result<(), String> {
    // NOTE: this re-emits the *current* snapshot (the coordinator re-notifies
    // the sink on `Refresh`), so the popover repaints with the latest known
    // state and catches up any missed live event. It does NOT trigger an
    // immediate provider re-poll or JSONL reparse - pollers run on their own
    // cadence. A true on-demand re-fetch (a refresh channel into the watcher
    // tasks) is the deferred "refresh_now mechanism" open sub-decision in
    // docs/superpowers/specs/2026-06-03-v0.3.0-popover-design.md (§12).
    handle
        .send(StateMsg::Refresh)
        .await
        .map_err(|e| e.to_string())
}

/// Return the non-secret settings (`settings.json` shape). Never includes any
/// API key - secrets live in the OS keychain, not here (AGENTS.md §3.4).
#[tauri::command]
pub fn get_settings() -> Result<Settings, String> {
    settings::load().map_err(|e| e.to_string())
}

/// Persist the non-secret settings atomically. Used for provider enable
/// toggles and the poll cadence. The watcher's pollers clamp the cadence to
/// the §3.1 floor regardless of what lands here, so a too-small value is safe.
#[tauri::command]
pub fn set_settings(settings: Settings) -> Result<(), String> {
    settings::save(&settings).map_err(|e| e.to_string())
}

/// Store a user-supplied API key in the OS keychain and mark its provider
/// configured. Only OpenAI keys are user-supplied - Anthropic uses Claude
/// Code's OAuth credential, which Balanze reads but never sets here.
///
/// Secret hygiene (§3.4): the key is validated, trimmed, written to the
/// keychain, and immediately dropped. It is never logged, echoed, or returned.
#[tauri::command]
pub fn set_api_key(provider: String, key: String) -> Result<(), String> {
    let key = prepare_api_key(&provider, &key)?;
    keychain::set(keychain::keys::OPENAI_API_KEY, key).map_err(|e| e.to_string())?;
    // Saving a key implies the user wants this provider polled. Flip the
    // enable flag so the watcher/CLI pick it up without a second action.
    let mut s = settings::load().map_err(|e| e.to_string())?;
    s.providers.openai_enabled = true;
    settings::save(&s).map_err(|e| e.to_string())?;
    Ok(())
}

/// Validate + normalize an inbound API key before it touches the keychain.
/// Returns the trimmed key on success. Pure (no I/O) so the validation
/// invariants are unit-testable; never logs or echoes the key.
fn prepare_api_key<'a>(provider: &str, key: &'a str) -> Result<&'a str, String> {
    if provider != "openai" {
        return Err(format!("unsupported provider: {provider}"));
    }
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("API key is empty".to_string());
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::prepare_api_key;

    #[test]
    fn prepare_api_key_rejects_unknown_provider() {
        let err = prepare_api_key("anthropic", "sk-anything").unwrap_err();
        assert!(err.contains("unsupported provider"));
    }

    #[test]
    fn prepare_api_key_rejects_empty_or_whitespace() {
        assert!(prepare_api_key("openai", "").is_err());
        assert!(prepare_api_key("openai", "   ").is_err());
    }

    #[test]
    fn prepare_api_key_trims_valid_key() {
        assert_eq!(
            prepare_api_key("openai", "  sk-admin-xyz  ").unwrap(),
            "sk-admin-xyz"
        );
    }
}
