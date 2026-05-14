//! The `Sink` trait — coordinator's side-effect boundary.
//!
//! The coordinator notifies the sink on every snapshot change and on every
//! source-level error. Concrete sinks decide what to do: log, emit a Tauri
//! event, repaint the tray, dedup against `last_painted` (see AGENTS.md
//! §3.1), persist to history, etc.
//!
//! Implementations must be `Send + 'static` because the sink lives inside the
//! coordinator's spawned tokio task. Methods are synchronous; tray/event
//! calls in Tauri are sync, and anything CPU-bound (color-bucket math,
//! string formatting) is fast.

use crate::messages::Source;
use crate::snapshot::Snapshot;

pub trait Sink: Send + 'static {
    /// Called after a successful merge into the snapshot, AND on every
    /// `Refresh` message. Implementations that need to dedup repaints
    /// (e.g., the production tray sink — `(ColorBucket, title_text)` per
    /// AGENTS.md §3.1) should track their own `last_painted` state and
    /// no-op when unchanged.
    fn on_snapshot(&mut self, snapshot: &Snapshot);

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
