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
//! statusLine: `get_statusline_status` / `set_statusline_wired` are sync
//! commands that delegate to `claude_statusline` (the only owner/writer of the
//! `statusLine` stanza in Claude Code's `settings.json`, boundary #12). They
//! enforce a no-clobber policy - Balanze never overwrites or strips another
//! tool's `statusLine`.
//!
//! All commands return `Result<_, String>` derived from `anyhow`/error
//! `to_string()`.

use claude_statusline::{
    STATUSLINE_INVOCATION, WireStatus, default_settings_path, locate_settings_path,
    read_wire_status, unwire_statusline, wire_statusline,
};
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
    // tasks) is a deferred "refresh_now mechanism" open sub-decision (a
    // refresh channel into the watcher tasks), not yet built.
    handle
        .send(StateMsg::Refresh)
        .await
        .map_err(|e| e.to_string())
}

/// Hide the popover window (ESC-to-dismiss). Window manipulation lives in Rust
/// (like the blur-hide, tray, and positioning), so the frontend asks via IPC
/// rather than holding a `core:window:allow-hide` capability - the webview gets
/// no window-mutation permission, only this one narrow action.
#[tauri::command]
pub fn hide_window(window: tauri::Window) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

/// Min/max popover window heights (logical px). The popover hugs content within
/// these bounds, so a one-provider view is shorter than a two-provider one.
pub const POPOVER_MIN_H: u32 = 220;
pub const POPOVER_MAX_H: u32 = 720;

/// Pure clamp so the bounds are unit-testable without a window.
pub fn clamp_popover_height(requested: u32) -> u32 {
    requested.clamp(POPOVER_MIN_H, POPOVER_MAX_H)
}

/// Resize the popover to hug its content height, then re-anchor it to the
/// tray/dock. Window manipulation lives in Rust (like `hide_window`), so the
/// webview holds no window capability. The width is preserved (only the height
/// follows content); the new height is clamped to the [`POPOVER_MIN_H`,
/// `POPOVER_MAX_H`] bounds before it is applied.
#[tauri::command]
pub fn resize_popover(window: tauri::WebviewWindow, height: u32) -> Result<(), String> {
    let h = clamp_popover_height(height);
    let size = window.inner_size().map_err(|e| e.to_string())?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    // Capture the OLD outer height before resizing: `reanchor_after_resize`
    // needs it to pin the pre-resize bottom edge on Windows/Linux (set_size
    // leaves the top-left fixed, so the bottom would otherwise drift down).
    let old_outer_h = window.outer_size().map_err(|e| e.to_string())?.height;
    window
        .set_size(tauri::LogicalSize::new(
            (size.width as f64 / scale).round(),
            h as f64,
        ))
        .map_err(|e| e.to_string())?;
    crate::reanchor_after_resize(&window, old_outer_h).map_err(|e| e.to_string())?;
    Ok(())
}

/// Return the non-secret settings (`settings.json` shape). Never includes any
/// API key - secrets live in the OS keychain, not here (AGENTS.md §3.4).
#[tauri::command]
pub fn get_settings() -> Result<Settings, String> {
    settings::load().map_err(|e| e.to_string())
}

/// Persist the non-secret settings atomically and live-apply them: provider
/// toggles and the poll cadence take effect without an app restart (see
/// [`apply_settings_live`]). The watcher's pollers clamp the cadence to the
/// §3.1 floor regardless of what lands here, so a too-small value is safe.
#[tauri::command]
pub fn set_settings(
    mut settings: Settings,
    coord: State<'_, StateCoordinatorHandle>,
    reload: State<'_, WatcherReload>,
) -> Result<(), String> {
    // `seen_welcome` is backend-owned first-run state, not a user setting; never
    // let a frontend settings write (provider toggles) reset it and re-trigger
    // the first-run welcome. Preserve the on-disk value over the inbound one.
    if let Ok(current) = settings::load() {
        settings.seen_welcome = current.seen_welcome;
    }
    settings::save(&settings).map_err(|e| e.to_string())?;
    apply_settings_live(&coord, &reload, settings);
    Ok(())
}

/// Store a user-supplied API key in the OS keychain and mark its provider
/// configured. Only OpenAI keys are user-supplied - Anthropic uses Claude
/// Code's OAuth credential, which Balanze reads but never sets here.
///
/// Secret hygiene (§3.4): the key is validated, trimmed, written to the
/// keychain, and immediately dropped. It is never logged, echoed, or returned.
#[tauri::command]
pub fn set_api_key(
    provider: String,
    key: String,
    coord: State<'_, StateCoordinatorHandle>,
    reload: State<'_, WatcherReload>,
) -> Result<(), String> {
    let key = prepare_api_key(&provider, &key)?;
    keychain::set(keychain::keys::OPENAI_API_KEY, key).map_err(|e| e.to_string())?;
    // Saving a key implies the user wants this provider polled. Flip the
    // enable flag so the watcher picks it up, then live-apply.
    let mut s = settings::load().map_err(|e| e.to_string())?;
    s.providers.openai_enabled = true;
    settings::save(&s).map_err(|e| e.to_string())?;
    apply_settings_live(&coord, &reload, s);
    Ok(())
}

/// Outcome of probing a user-supplied API key against the provider WITHOUT
/// storing it, so the settings UI can give immediate feedback instead of the
/// user waiting up to a full poll interval to learn a key is wrong.
/// `ok` = the key authenticated. `retryable` = the check failed transiently
/// (network / rate limit), so the UI may offer "save anyway"; a non-retryable
/// failure means the key is definitively wrong and should not be stored.
#[derive(serde::Serialize)]
pub struct ApiKeyValidation {
    pub ok: bool,
    pub retryable: bool,
    pub message: Option<String>,
}

/// Probe a user-supplied API key against the provider without storing it. Async
/// (unlike the other key commands): it makes one fail-fast network request, the
/// same month-to-date costs call the poller uses, so a key that validates here
/// works for the real poll. Secret hygiene (§3.4): the key is never logged,
/// echoed, or persisted by this command.
#[tauri::command]
pub async fn validate_api_key(provider: String, key: String) -> Result<ApiKeyValidation, String> {
    let key = prepare_api_key(&provider, &key)?.to_string();
    Ok(match watcher::validate_openai_key(&key).await {
        watcher::KeyProbe::Valid => ApiKeyValidation {
            ok: true,
            retryable: false,
            message: None,
        },
        watcher::KeyProbe::Rejected(msg) => ApiKeyValidation {
            ok: false,
            retryable: false,
            message: Some(msg),
        },
        watcher::KeyProbe::Unreachable(msg) => ApiKeyValidation {
            ok: false,
            retryable: true,
            message: Some(msg),
        },
    })
}

/// Whether a user-supplied API key for `provider` exists in the OS keychain.
/// Lets the settings UI show a "key configured" affordance without ever reading
/// the key value. (Does not consider the `BALANZE_OPENAI_KEY` env override - the
/// UI affordance manages the keychain-stored key.)
#[tauri::command]
pub fn has_api_key(provider: String) -> Result<bool, String> {
    if provider != "openai" {
        return Err(format!("unsupported provider: {provider}"));
    }
    keychain::exists(keychain::keys::OPENAI_API_KEY).map_err(|e| e.to_string())
}

/// Remove a user-supplied API key from the keychain and disable its provider,
/// then live-apply (the cell clears). Idempotent - succeeds if no key existed.
#[tauri::command]
pub fn clear_api_key(
    provider: String,
    coord: State<'_, StateCoordinatorHandle>,
    reload: State<'_, WatcherReload>,
) -> Result<(), String> {
    if provider != "openai" {
        return Err(format!("unsupported provider: {provider}"));
    }
    keychain::delete(keychain::keys::OPENAI_API_KEY).map_err(|e| e.to_string())?;
    let mut s = settings::load().map_err(|e| e.to_string())?;
    s.providers.openai_enabled = false;
    settings::save(&s).map_err(|e| e.to_string())?;
    apply_settings_live(&coord, &reload, s);
    Ok(())
}

/// Sender that asks the host's watcher supervisor to re-spawn its tasks with
/// the latest settings. Managed in Tauri state by `boot_backend`.
pub struct WatcherReload(pub tokio::sync::mpsc::Sender<()>);

/// Live-apply a settings change without an app restart, in two parts:
/// 1. Tell the coordinator (owner of the `Snapshot`) to reset the cells of any
///    now-disabled provider via `StateMsg::SettingsChanged`.
/// 2. Signal the watcher supervisor to re-spawn its tasks, so enabled providers
///    start polling and disabled ones stop.
///
/// Both sends are non-blocking and best-effort: a full or closed channel just
/// means the change applies on the next natural cycle / restart, never an error
/// to the user (the settings are already persisted by the time we get here).
fn apply_settings_live(coord: &StateCoordinatorHandle, reload: &WatcherReload, settings: Settings) {
    let _ = coord.try_send(StateMsg::SettingsChanged(settings));
    let _ = reload.0.try_send(());
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

/// Whether Claude Code's `statusLine` is wired to Balanze, free, or taken by
/// another command. Serialized to the frontend as `{ status, command }`.
#[derive(serde::Serialize)]
pub struct StatuslineWire {
    /// `"wired"` (ours) | `"unwired"` (free) | `"occupied"` (another command).
    status: &'static str,
    /// The occupying command when `status == "occupied"`, else `null`.
    command: Option<String>,
}

/// Report whether Claude Code's `statusLine` is wired to Balanze.
#[tauri::command]
pub fn get_statusline_status() -> Result<StatuslineWire, String> {
    let path = locate_settings_path().unwrap_or_else(|_| default_settings_path());
    Ok(match read_wire_status(&path).map_err(|e| e.to_string())? {
        WireStatus::WiredToBalanze => StatuslineWire {
            status: "wired",
            command: None,
        },
        WireStatus::Unwired => StatuslineWire {
            status: "unwired",
            command: None,
        },
        WireStatus::OccupiedBy(cmd) => StatuslineWire {
            status: "occupied",
            command: Some(cmd),
        },
    })
}

/// Wire (`wired = true`) or unwire (`wired = false`) Balanze's `statusLine` in
/// Claude Code's `settings.json`. No-clobber: refuses to overwrite a stanza set
/// to another command, and only removes the stanza when it is ours.
#[tauri::command]
pub fn set_statusline_wired(wired: bool) -> Result<(), String> {
    let path = locate_settings_path().unwrap_or_else(|_| default_settings_path());
    let status = read_wire_status(&path).map_err(|e| e.to_string())?;
    match plan_statusline_action(wired, &status) {
        StatuslineAction::Wire => {
            wire_statusline(&path, STATUSLINE_INVOCATION).map_err(|e| e.to_string())
        }
        StatuslineAction::Unwire => unwire_statusline(&path).map_err(|e| e.to_string()),
        StatuslineAction::NoOp => Ok(()),
        StatuslineAction::RefuseOccupied(cmd) => Err(format!(
            "Claude Code's statusLine is set to another command ({cmd}); not overwriting it"
        )),
    }
}

#[derive(Debug, PartialEq)]
enum StatuslineAction {
    Wire,
    Unwire,
    NoOp,
    RefuseOccupied(String),
}

/// Pure no-clobber policy: given the requested wired state and the current
/// status, decide the action. Balanze only ever writes or removes its own
/// stanza - never overwrites or strips another tool's `statusLine`.
fn plan_statusline_action(wired: bool, status: &WireStatus) -> StatuslineAction {
    match (wired, status) {
        // Don't overwrite someone else's command.
        (true, WireStatus::OccupiedBy(cmd)) => StatuslineAction::RefuseOccupied(cmd.clone()),
        // Unwired or already ours: (re)wire - idempotent.
        (true, _) => StatuslineAction::Wire,
        // Only remove a stanza we own.
        (false, WireStatus::WiredToBalanze) => StatuslineAction::Unwire,
        // Not ours / absent: nothing to remove.
        (false, _) => StatuslineAction::NoOp,
    }
}

#[cfg(test)]
mod tests {
    use super::{StatuslineAction, plan_statusline_action, prepare_api_key};
    use claude_statusline::WireStatus;

    #[test]
    fn clamp_popover_height_bounds() {
        use super::{POPOVER_MAX_H, POPOVER_MIN_H, clamp_popover_height};
        assert_eq!(clamp_popover_height(10), POPOVER_MIN_H);
        assert_eq!(clamp_popover_height(99_999), POPOVER_MAX_H);
        let mid = (POPOVER_MIN_H + POPOVER_MAX_H) / 2;
        assert_eq!(clamp_popover_height(mid), mid);
    }

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

    #[test]
    fn wire_request_wires_when_free_or_ours() {
        assert_eq!(
            plan_statusline_action(true, &WireStatus::Unwired),
            StatuslineAction::Wire
        );
        // Idempotent: re-wiring our own stanza is still a Wire.
        assert_eq!(
            plan_statusline_action(true, &WireStatus::WiredToBalanze),
            StatuslineAction::Wire
        );
    }

    #[test]
    fn wire_request_refuses_to_clobber_another_command() {
        assert_eq!(
            plan_statusline_action(true, &WireStatus::OccupiedBy("other-tool".to_string())),
            StatuslineAction::RefuseOccupied("other-tool".to_string())
        );
    }

    #[test]
    fn unwire_request_only_removes_our_own_stanza() {
        assert_eq!(
            plan_statusline_action(false, &WireStatus::WiredToBalanze),
            StatuslineAction::Unwire
        );
        // Not ours / absent: leave it alone.
        assert_eq!(
            plan_statusline_action(false, &WireStatus::Unwired),
            StatuslineAction::NoOp
        );
        assert_eq!(
            plan_statusline_action(false, &WireStatus::OccupiedBy("other-tool".to_string())),
            StatuslineAction::NoOp
        );
    }
}
