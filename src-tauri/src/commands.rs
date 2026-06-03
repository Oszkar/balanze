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
    handle
        .send(StateMsg::Refresh)
        .await
        .map_err(|e| e.to_string())
}
