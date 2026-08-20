//! State coordinator - the actor that owns Balanze's in-memory `Snapshot`.
//!
//! Per AGENTS.md §4 #7: this crate is the ONLY writer of the in-memory
//! `Snapshot` AND (when wired with a `TauriSink`) the ONLY caller of OS tray
//! APIs. Pollers (the future `watcher`, `anthropic_oauth`, `openai_client`)
//! send `StateMsg::Update(SourceUpdate)` to the coordinator; the coordinator
//! merges into the `Snapshot`, then notifies the `Sink` for side effects
//! (Tauri event emit, tray repaint).
//!
//! ## Layering
//!
//! ```text
//!   pollers ──Update──┐
//!   tray ticker ─Refresh─┤      ┌─────────────────────┐
//!   Tauri ──Query────────┼──>──>│  StateCoordinator   │──>── Sink
//!   settings ─Changed────┘      │   owns Snapshot     │  (Tauri / LogSink)
//!                                └─────────────────────┘
//! ```
//!
//! The Sink trait is the side-effect boundary. For unit tests use a `NullSink`
//! or a custom test sink; for production behind Tauri, src-tauri provides a
//! `TauriSink` that calls `app.emit("usage_updated", ...)` and
//! `tray.set_icon(...)` / `tray.set_title(...)`. The coordinator itself
//! doesn't depend on Tauri.

mod coordinator;
mod jsonl;
mod messages;
mod sink;
mod sink_file;
mod snapshot;
pub mod snapshot_file;

#[cfg(test)]
mod test_support;

pub use codex_local::WindowKind;
pub use coordinator::{StateCoordinatorHandle, spawn, spawn_with_optional_file};
pub use jsonl::{JsonlCells, summarize_jsonl};
pub use messages::{
    ClaudeJsonlInput, Source, SourcePartial, SourceUpdate, StateMsg, WatcherGeneration,
};
pub use sink::{LogSink, NullSink, Sink};
pub use snapshot::{
    AnthropicQuotaSource, JsonlSnapshot, SNAPSHOT_SCHEMA_VERSION, STATUSLINE_FRESHNESS_SECS,
    Snapshot, WindowPace, pace_for_oauth, record_error,
};
pub use snapshot_file::{
    SnapshotFileError, SnapshotFilePayload, atomic_write_snapshot_file, read_snapshot_file,
    snapshot_file_path,
};
