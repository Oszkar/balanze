//! Tauri command surface for v0.3.0 (IPC contract, AGENTS.md §4 #9):
//! `get_snapshot` + `refresh_now`. Both reach the coordinator via the
//! managed `StateCoordinatorHandle`. Commands return `Result<_, String>`.

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
    // immediate provider re-poll or JSONL reparse — pollers run on their own
    // cadence. A true on-demand re-fetch (a refresh channel into the watcher
    // tasks) is the deferred "refresh_now mechanism" open sub-decision in
    // docs/superpowers/specs/2026-06-03-v0.3.0-popover-design.md (§12).
    handle
        .send(StateMsg::Refresh)
        .await
        .map_err(|e| e.to_string())
}
