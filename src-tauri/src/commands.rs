//! Tauri command surface (IPC contract, AGENTS.md §4 #9).
//!
//! Read/refresh: `get_snapshot` + `refresh_now` reach the coordinator via the
//! managed `StateCoordinatorHandle` (async - they await the coordinator).
//!
//! Settings, keychain, and statusline commands are async commands whose
//! synchronous work runs through `spawn_blocking`, so filesystem, subprocess,
//! and keychain latency never stalls a tokio worker. Secret hygiene (§3.4): the API key
//! is never logged or echoed, and `get_settings` returns only the non-secret
//! `Settings` shape - the key itself never crosses back to the frontend.
//!
//! statusLine: `get_statusline_status` / `set_statusline_wired` /
//! `replace_statusline` / `restore_statusline` are sync commands that delegate
//! to `claude_statusline` (the only owner/writer of the `statusLine` stanza in
//! Claude Code's `settings.json`, boundary #12). `set_statusline_wired` enforces
//! a no-clobber policy - it never overwrites or strips another tool's
//! `statusLine`. `replace_statusline` is the explicit, consent-driven override:
//! it backs the foreign command up to `settings.statusline.replaced_command`
//! before wiring (rolling back on failure); `restore_statusline` writes that
//! backup back (or unwires only Balanze's own line), then clears it.
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

async fn run_blocking<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("blocking worker failed: {error}"))?
}

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

fn resized_outer_height(logical_height: u32, scale_factor: f64) -> u32 {
    (logical_height as f64 * scale_factor).round().max(1.0) as u32
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
    let old_outer = window.outer_size().map_err(|e| e.to_string())?;
    let new_outer = tauri::PhysicalSize::new(old_outer.width, resized_outer_height(h, scale));
    window
        .set_size(tauri::LogicalSize::new(
            (size.width as f64 / scale).round(),
            h as f64,
        ))
        .map_err(|e| e.to_string())?;
    crate::reanchor_after_resize(&window, old_outer.height, new_outer)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Return the non-secret settings (`settings.json` shape). Never includes any
/// API key - secrets live in the OS keychain, not here (AGENTS.md §3.4).
#[tauri::command]
pub async fn get_settings() -> Result<Settings, String> {
    run_blocking(|| settings::load().map_err(|e| e.to_string())).await
}

/// Persist the non-secret settings atomically and live-apply them: provider
/// toggles and the poll cadence take effect without an app restart (see
/// [`apply_settings_live`]). The watcher's pollers clamp the cadence to the
/// §3.1 floor regardless of what lands here, so a too-small value is safe.
#[tauri::command]
pub async fn set_settings(
    mut settings: Settings,
    reload: State<'_, WatcherReload>,
) -> Result<(), String> {
    // `seen_welcome` and `statusline` are backend-owned, not user settings the
    // frontend edits: `seen_welcome` is first-run state, and `statusline` (incl.
    // the `replaced_command` backup) is mutated out of band by the replace /
    // restore commands and has no frontend editor. A frontend settings write
    // (provider toggles) round-trips a stale copy, so preserve the on-disk
    // values over the inbound ones - otherwise a toggle after a Replace would
    // silently wipe the backup.
    // Load the on-disk copy to preserve the backend-owned fields. On a corrupt
    // settings.json, bail instead of proceeding: the inbound copy carries a
    // stale/default `statusline`, so saving it would wipe the replaced_command
    // backup - the same clobber the read-only save paths guard against.
    let settings = run_blocking(move || {
        let current = settings::load_for_update()
            .map_err(|e| format!("{}: {e}", settings::UPDATE_LOAD_HINT))?;
        settings.seen_welcome = current.seen_welcome;
        settings.statusline = current.statusline;
        settings::save(&settings).map_err(|e| e.to_string())?;
        Ok(settings)
    })
    .await?;
    apply_settings_live(&reload, settings).await
}

/// Store a user-supplied API key in the OS keychain and mark its provider
/// configured. Only OpenAI keys are user-supplied - Anthropic uses Claude
/// Code's OAuth credential, which Balanze reads but never sets here.
///
/// Secret hygiene (§3.4): the key is validated, trimmed, written to the
/// keychain, and immediately dropped. It is never logged, echoed, or returned.
#[tauri::command]
pub async fn set_api_key(
    provider: String,
    key: String,
    reload: State<'_, WatcherReload>,
) -> Result<(), String> {
    let key = prepare_api_key(&provider, &key)?.to_string();
    // Saving a key implies the user wants this provider polled. Flip the
    // enable flag so the watcher picks it up, then live-apply.
    let s = run_blocking(move || {
        keychain::set(keychain::keys::OPENAI_API_KEY, &key).map_err(|e| e.to_string())?;
        let mut s = settings::load().map_err(|e| e.to_string())?;
        s.providers.openai_enabled = true;
        settings::save(&s).map_err(|e| e.to_string())?;
        Ok(s)
    })
    .await?;
    apply_settings_live(&reload, s).await
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
pub async fn has_api_key(provider: String) -> Result<bool, String> {
    if provider != "openai" {
        return Err(format!("unsupported provider: {provider}"));
    }
    run_blocking(|| keychain::exists(keychain::keys::OPENAI_API_KEY).map_err(|e| e.to_string()))
        .await
}

/// Remove a user-supplied API key from the keychain and disable its provider,
/// then live-apply (the cell clears). Idempotent - succeeds if no key existed.
#[tauri::command]
pub async fn clear_api_key(
    provider: String,
    reload: State<'_, WatcherReload>,
) -> Result<(), String> {
    if provider != "openai" {
        return Err(format!("unsupported provider: {provider}"));
    }
    let s = run_blocking(|| {
        keychain::delete(keychain::keys::OPENAI_API_KEY).map_err(|e| e.to_string())?;
        let mut s = settings::load().map_err(|e| e.to_string())?;
        s.providers.openai_enabled = false;
        settings::save(&s).map_err(|e| e.to_string())?;
        Ok(s)
    })
    .await?;
    apply_settings_live(&reload, s).await
}

/// Sender that asks the host's watcher supervisor to re-spawn its tasks with
/// the latest settings. Managed in Tauri state by `boot_backend`.
pub struct SettingsTransition {
    pub settings: Settings,
    pub applied: tokio::sync::oneshot::Sender<Result<(), String>>,
}

pub struct WatcherReload(pub tokio::sync::mpsc::Sender<SettingsTransition>);

/// Await one supervised settings transition. The supervisor joins the old
/// pollers, advances the coordinator generation, starts the replacement task
/// set, and only then completes the reply. A closed supervisor is an explicit
/// command error, never a silently dropped live update.
async fn apply_settings_live(reload: &WatcherReload, settings: Settings) -> Result<(), String> {
    let (applied, completed) = tokio::sync::oneshot::channel();
    reload
        .0
        .send(SettingsTransition { settings, applied })
        .await
        .map_err(|_| "watcher settings supervisor has shut down".to_string())?;
    completed
        .await
        .map_err(|_| "watcher settings supervisor dropped the transition".to_string())?
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
/// another command. Serialized to the frontend as `{ status, command,
/// replaced_command }`.
#[derive(serde::Serialize)]
pub struct StatuslineWire {
    /// `"wired"` (ours) | `"unwired"` (free) | `"occupied"` (another command).
    status: &'static str,
    /// The occupying command when `status == "occupied"`, else `null`.
    command: Option<String>,
    /// The foreign command Balanze displaced (from Balanze settings), if any.
    /// Non-null means a restore is available via `restore_statusline`.
    replaced_command: Option<String>,
}

/// Report whether Claude Code's `statusLine` is wired to Balanze.
#[tauri::command]
pub async fn get_statusline_status() -> Result<StatuslineWire, String> {
    run_blocking(|| {
        let path = locate_settings_path().unwrap_or_else(|_| default_settings_path());
        let status = read_wire_status(&path).map_err(|e| e.to_string())?;
        let replaced_command = settings::load()
            .ok()
            .and_then(|s| s.statusline.replaced_command);
        Ok(match status {
            WireStatus::WiredToBalanze => StatuslineWire {
                status: "wired",
                command: None,
                replaced_command,
            },
            WireStatus::Unwired => StatuslineWire {
                status: "unwired",
                command: None,
                replaced_command,
            },
            WireStatus::OccupiedBy(cmd) => StatuslineWire {
                status: "occupied",
                command: Some(cmd),
                replaced_command,
            },
        })
    })
    .await
}

/// Replace a foreign `statusLine.command` with Balanze's, backing the foreign
/// command up to `settings.statusline.replaced_command` so it can be restored
/// later. If the statusLine is already Balanze's or unwired, wires it directly.
/// Provider-agnostic: keys only off `OccupiedBy(cmd)` - never reads or touches
/// the foreign tool's own config files.
#[tauri::command]
pub async fn replace_statusline() -> Result<(), String> {
    run_blocking(|| {
        let path = locate_settings_path().unwrap_or_else(|_| default_settings_path());
        let mut s = settings::load_for_update()
            .map_err(|e| format!("{}: {e}", settings::UPDATE_LOAD_HINT))?;
        let prior = s.statusline.replaced_command.clone();
        if let WireStatus::OccupiedBy(cmd) = read_wire_status(&path).map_err(|e| e.to_string())? {
            // Don't back up the "statusLine present but no usable command" sentinel;
            // it is not restorable.
            if cmd != claude_statusline::NON_STRING_STATUSLINE_COMMAND {
                s.statusline.replaced_command = Some(cmd);
                settings::save(&s).map_err(|e| e.to_string())?;
            }
        }
        if let Err(e) = wire_statusline(&path, STATUSLINE_INVOCATION) {
            // Roll back to the PRIOR backup (not None) so a failed replace never wipes
            // an existing one, and the UI shows no phantom Restore for this attempt.
            s.statusline.replaced_command = prior;
            let _ = settings::save(&s);
            return Err(e.to_string());
        }
        Ok(())
    })
    .await
}

/// Restore the foreign `statusLine.command` that Balanze displaced via
/// `replace_statusline`: write the backup back (or unwire Balanze's own line if
/// the backup is `None`), then clear the backup. If a foreign command now
/// occupies the stanza (one Balanze did not displace), it is left untouched and
/// the backup is KEPT, surfaced as an error to the caller.
#[tauri::command]
pub async fn restore_statusline() -> Result<(), String> {
    run_blocking(|| {
        let path = locate_settings_path().unwrap_or_else(|_| default_settings_path());
        let mut s = settings::load_for_update()
            .map_err(|e| format!("{}: {e}", settings::UPDATE_LOAD_HINT))?;
        let previous = s.statusline.replaced_command.take();
        // Fully-qualified to disambiguate from this Tauri command of the same name.
        let wrote = claude_statusline::restore_statusline(&path, previous.as_deref())
            .map_err(|e| e.to_string())?;
        if wrote {
            // Backup consumed - persist the cleared value.
            settings::save(&s).map_err(|e| e.to_string())
        } else if previous.is_some() {
            // A foreign command owns the stanza; keep the backup (do not save the
            // cleared value) and tell the caller.
            Err("Claude Code's statusLine is set to another command; not overwriting it. Your backup is kept.".to_string())
        } else {
            Ok(())
        }
    })
    .await
}

/// Wire (`wired = true`) or unwire (`wired = false`) Balanze's `statusLine` in
/// Claude Code's `settings.json`. No-clobber: refuses to overwrite a stanza set
/// to another command, and only removes the stanza when it is ours.
#[tauri::command]
pub async fn set_statusline_wired(wired: bool) -> Result<(), String> {
    run_blocking(move || {
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
    })
    .await
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
    fn resized_outer_height_uses_requested_logical_height() {
        assert_eq!(super::resized_outer_height(333, 1.0), 333);
        assert_eq!(super::resized_outer_height(333, 1.5), 500);
        assert_eq!(super::resized_outer_height(333, 2.0), 666);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_operations_run_on_blocking_worker() {
        let runtime_thread = std::thread::current().id();
        let worker_thread = super::run_blocking(|| Ok(std::thread::current().id()))
            .await
            .unwrap();
        assert_ne!(runtime_thread, worker_thread);
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
