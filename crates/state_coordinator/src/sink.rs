//! The `Sink` trait - coordinator's side-effect boundary.
//!
//! The coordinator notifies the sink on every snapshot change and on every
//! source-level error. Concrete sinks decide what to do: log, emit a Tauri
//! event, enqueue a tray repaint target, persist to history, etc.
//!
//! Implementations must be `Send + 'static` because the sink lives inside the
//! coordinator's spawned tokio task. Methods are synchronous and must return
//! quickly. Blocking OS/main-thread tray calls belong behind a dedicated
//! worker; color-bucket math and string formatting are fast enough inline.

use crate::messages::Source;
use crate::snapshot::Snapshot;

pub trait Sink: Send + 'static {
    /// Called after a successful merge into the snapshot. Implementations that
    /// enqueue repaints should dedup their target before sending it to a worker.
    fn on_snapshot(&mut self, snapshot: &Snapshot);

    /// Presentation-only re-notification of unchanged source state. The default
    /// preserves ordinary sink behavior; durability wrappers override this so
    /// a UI/TUI repaint does not fsync an identical `snapshot.json`.
    fn on_refresh(&mut self, snapshot: &Snapshot) {
        self.on_snapshot(snapshot);
    }

    /// Called when snapshot state must be durably published without notifying
    /// presentation sinks. Error-slot transitions use this path so
    /// `snapshot.json` stays current without re-emitting an unchanged data
    /// snapshot to the frontend on every failed provider poll.
    fn on_snapshot_durable(&mut self, _snapshot: &Snapshot) {}

    /// Called when an `Update` arrives with `result: Err(...)`. The
    /// snapshot's `_error` slot is already set before this is called.
    fn on_degraded(&mut self, source: Source, error: &str);
}

/// Discards all events. Useful for tests where the sink isn't the subject.
#[derive(Debug, Default)]
pub struct NullSink;

impl Sink for NullSink {
    fn on_snapshot(&mut self, _snapshot: &Snapshot) {}
    fn on_degraded(&mut self, _source: Source, _error: &str) {}
}

/// Emits tracing log lines for every event. Convenient default while the
/// production Tauri sink doesn't exist yet.
#[derive(Debug, Default)]
pub struct LogSink;

impl Sink for LogSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        tracing::debug!(
            "state_coordinator: snapshot updated; fetched_at={}",
            snapshot.fetched_at
        );
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        tracing::warn!("state_coordinator: source {source:?} degraded: {error}");
    }
}
