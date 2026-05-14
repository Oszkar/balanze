//! The `StateCoordinator` actor. Runs in a dedicated tokio task; receives
//! `StateMsg`s on a bounded mpsc channel; owns the in-memory `Snapshot`;
//! notifies a `Sink` for side effects.

use chrono::Utc;
use settings::Settings;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::messages::{SourceUpdate, StateMsg};
use crate::sink::Sink;
use crate::snapshot::{merge_partial, record_error, Snapshot};

/// Default mpsc capacity. AGENTS.md §3.2 mentions a "dropped state-coordinator
/// mpsc message" warning case for senders that use `try_send`; for normal
/// `.send().await` callers, this is the backpressure threshold.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;

/// A handle to a running coordinator. Cloning a handle clones its sender,
/// so multiple producers (pollers, tray ticker, Tauri commands) can share
/// one coordinator.
#[derive(Debug, Clone)]
pub struct StateCoordinatorHandle {
    tx: mpsc::Sender<StateMsg>,
}

impl StateCoordinatorHandle {
    /// Send a message to the coordinator. Awaits backpressure if the channel
    /// is full. Returns `Err` only if the coordinator has shut down.
    pub async fn send(&self, msg: StateMsg) -> Result<(), mpsc::error::SendError<StateMsg>> {
        self.tx.send(msg).await
    }

    /// Non-blocking send. Returns `Err(TrySendError::Full)` if the channel
    /// is saturated — caller should log and continue rather than queue.
    ///
    /// `TrySendError` is large because it includes the un-sent `StateMsg`
    /// payload; we box it so this `Result` stays cheap at the call site.
    pub fn try_send(
        &self,
        msg: StateMsg,
    ) -> Result<(), Box<mpsc::error::TrySendError<StateMsg>>> {
        self.tx.try_send(msg).map_err(Box::new)
    }

    /// Read the current snapshot. Convenience wrapper for `Query` with a
    /// oneshot reply channel.
    pub async fn query(&self) -> Result<Snapshot, QueryError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(StateMsg::Query(reply_tx))
            .await
            .map_err(|_| QueryError::CoordinatorClosed)?;
        reply_rx.await.map_err(|_| QueryError::CoordinatorClosed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("coordinator task has shut down")]
    CoordinatorClosed,
}

/// Spawn the coordinator actor on the current tokio runtime.
///
/// Returns `(handle, join)`:
/// - `handle` — clone to get more senders.
/// - `join` — the JoinHandle for the spawned task. AGENTS.md §3.2 says
///   long-running tasks should be supervised with this in a `tokio::select!`.
///   Tests can drop the handle to shut down the coordinator and await `join`.
pub fn spawn<S: Sink>(sink: S) -> (StateCoordinatorHandle, JoinHandle<()>) {
    spawn_with_capacity(sink, DEFAULT_CHANNEL_CAPACITY)
}

/// Same as `spawn` but with a custom channel capacity (used by the saturation
/// test).
pub fn spawn_with_capacity<S: Sink>(
    sink: S,
    capacity: usize,
) -> (StateCoordinatorHandle, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<StateMsg>(capacity);
    let join = tokio::spawn(run_loop(rx, sink));
    (StateCoordinatorHandle { tx }, join)
}

async fn run_loop<S: Sink>(mut rx: mpsc::Receiver<StateMsg>, mut sink: S) {
    let mut snapshot = Snapshot::empty(Utc::now());
    let mut _last_settings: Option<Settings> = None;
    while let Some(msg) = rx.recv().await {
        handle_msg(&mut snapshot, &mut sink, &mut _last_settings, msg);
    }
    tracing::debug!("state_coordinator: channel closed, shutting down");
}

fn handle_msg<S: Sink>(
    snapshot: &mut Snapshot,
    sink: &mut S,
    last_settings: &mut Option<Settings>,
    msg: StateMsg,
) {
    match msg {
        StateMsg::Update(SourceUpdate { source, result }) => match result {
            Ok(partial) => {
                // Defensive: `partial.source()` and `source` should agree.
                // If they don't, trust the variant of `partial` (it carries
                // the data) and warn.
                if partial.source() != source {
                    tracing::warn!(
                        "state_coordinator: Update.source {source:?} disagrees with payload {:?}; using payload",
                        partial.source()
                    );
                }
                merge_partial(snapshot, partial);
                snapshot.fetched_at = Utc::now();
                sink.on_snapshot(snapshot);
            }
            Err(err) => {
                record_error(snapshot, source, &err);
                sink.on_degraded(source, &err);
            }
        },
        StateMsg::Query(reply) => {
            // Receiver dropped → caller gave up; nothing to do.
            let _ = reply.send(snapshot.clone());
        }
        StateMsg::Refresh => {
            // Re-notify with current state. Sinks that need to repaint will
            // do so; sinks that dedup against `last_painted` will no-op.
            sink.on_snapshot(snapshot);
        }
        StateMsg::SettingsChanged(s) => {
            // Scaffold: just remember it. Future work wires this to pollers
            // via a settings-change broadcast.
            *last_settings = Some(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{Source, SourcePartial, SourceUpdate};
    use crate::sink::{NullSink, Sink};
    use crate::snapshot::Snapshot;
    use crate::test_support::{jsonl_snapshot, oauth_snapshot, openai_costs};
    use std::sync::{Arc, Mutex};

    /// Test sink: records every event so the test can assert on them.
    #[derive(Debug, Default, Clone)]
    struct RecordingSink {
        inner: Arc<Mutex<RecordingInner>>,
    }
    #[derive(Debug, Default)]
    struct RecordingInner {
        snapshots: Vec<Snapshot>,
        errors: Vec<(Source, String)>,
    }
    impl RecordingSink {
        fn snapshot_count(&self) -> usize {
            self.inner.lock().unwrap().snapshots.len()
        }
        fn error_count(&self) -> usize {
            self.inner.lock().unwrap().errors.len()
        }
        fn last_error(&self) -> Option<(Source, String)> {
            self.inner.lock().unwrap().errors.last().cloned()
        }
    }
    impl Sink for RecordingSink {
        fn on_snapshot(&mut self, snapshot: &Snapshot) {
            self.inner.lock().unwrap().snapshots.push(snapshot.clone());
        }
        fn on_degraded(&mut self, source: Source, error: &str) {
            self.inner
                .lock()
                .unwrap()
                .errors
                .push((source, error.to_string()));
        }
    }

    #[tokio::test]
    async fn update_msg_merges_data_and_notifies_sink() {
        let sink = RecordingSink::default();
        let (handle, _join) = spawn(sink.clone());

        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeOAuth,
                result: Ok(SourcePartial::ClaudeOAuth(oauth_snapshot())),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        assert!(snap.claude_oauth.is_some());
        assert!(snap.claude_oauth_error.is_none());
        // Sink saw exactly one on_snapshot (from the Update).
        assert_eq!(sink.snapshot_count(), 1);
        assert_eq!(sink.error_count(), 0);
    }

    #[tokio::test]
    async fn update_msg_with_err_records_error_and_calls_on_degraded() {
        let sink = RecordingSink::default();
        let (handle, _join) = spawn(sink.clone());

        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::OpenAiCosts,
                result: Err("network unreachable".to_string()),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        assert_eq!(
            snap.openai_error.as_deref(),
            Some("network unreachable")
        );
        assert!(snap.openai.is_none(), "no data on error");
        assert_eq!(sink.snapshot_count(), 0);
        assert_eq!(sink.error_count(), 1);
        let (src, msg) = sink.last_error().unwrap();
        assert_eq!(src, Source::OpenAiCosts);
        assert_eq!(msg, "network unreachable");
    }

    #[tokio::test]
    async fn query_msg_returns_current_snapshot() {
        let (handle, _join) = spawn(NullSink);
        // Seed the snapshot with one update.
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Ok(SourcePartial::ClaudeJsonl(jsonl_snapshot())),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        assert!(snap.claude_jsonl.is_some());
        assert_eq!(snap.claude_jsonl.as_ref().unwrap().files_scanned, 5);
    }

    #[tokio::test]
    async fn refresh_msg_re_notifies_sink_with_current_state() {
        let sink = RecordingSink::default();
        let (handle, _join) = spawn(sink.clone());

        // No data merged; Refresh still re-notifies (sink dedups if it cares).
        handle.send(StateMsg::Refresh).await.unwrap();
        let _ = handle.query().await.unwrap();

        assert_eq!(sink.snapshot_count(), 1, "Refresh should call on_snapshot");
    }

    #[tokio::test]
    async fn settings_changed_msg_does_not_panic() {
        let (handle, _join) = spawn(NullSink);
        let s = Settings::default();
        handle.send(StateMsg::SettingsChanged(s)).await.unwrap();
        // Followed by a Query to confirm the actor is still alive and processing:
        let _ = handle.query().await.unwrap();
    }

    #[tokio::test]
    async fn mpsc_processes_burst_in_order_no_drops() {
        // Tight channel + a burst of updates. Bounded mpsc applies
        // backpressure to senders rather than dropping messages, so all N
        // updates must arrive and the final snapshot reflects the last one.
        let sink = RecordingSink::default();
        let (handle, _join) = spawn_with_capacity(sink.clone(), 4);

        const N: usize = 32;
        for i in 0..N {
            let mut openai = openai_costs();
            openai.total_usd = i as f64;
            handle
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::OpenAiCosts,
                    result: Ok(SourcePartial::OpenAiCosts(openai)),
                }))
                .await
                .unwrap();
        }

        let snap = handle.query().await.unwrap();
        assert_eq!(
            snap.openai.as_ref().unwrap().total_usd,
            (N - 1) as f64,
            "last update wins; no message was dropped"
        );
        assert_eq!(sink.snapshot_count(), N, "every update reached the sink");
    }

    #[tokio::test]
    async fn handle_clone_shares_underlying_coordinator() {
        let (handle, _join) = spawn(NullSink);
        let handle_b = handle.clone();

        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeOAuth,
                result: Ok(SourcePartial::ClaudeOAuth(oauth_snapshot())),
            }))
            .await
            .unwrap();

        let snap = handle_b.query().await.unwrap();
        assert!(
            snap.claude_oauth.is_some(),
            "clone sees the same coordinator's state"
        );
    }
}
