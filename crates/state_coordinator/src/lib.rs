//! State coordinator — the actor that owns Balanze's in-memory `Snapshot`.
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
mod messages;
mod sink;
mod snapshot;

#[cfg(test)]
mod test_support;

pub use coordinator::{spawn, StateCoordinatorHandle};
pub use messages::{Source, SourcePartial, SourceUpdate, StateMsg};
pub use sink::{LogSink, NullSink, Sink};
pub use snapshot::{merge_partial, record_error, JsonlSnapshot, Snapshot};
