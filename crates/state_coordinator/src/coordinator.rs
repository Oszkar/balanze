//! The `StateCoordinator` actor. Runs in a dedicated tokio task; receives
//! `StateMsg`s on a bounded mpsc channel; owns the in-memory `Snapshot`;
//! notifies a `Sink` for side effects.

use std::sync::Arc;

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::Utc;
use claude_cost::PriceTable;
use claude_parser::UsageEvent;
use settings::Settings;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::jsonl::summarize_jsonl;
use crate::messages::{Source, SourcePartial, SourceUpdate, StateMsg};
use crate::sink::Sink;
use crate::snapshot::{Snapshot, clear_source, pace_for_oauth, record_error};

/// Mutable state owned by the coordinator's single tokio task. Grouped
/// into one struct so `handle_msg` takes one `&mut` instead of threading
/// several. Never crosses a thread boundary — only `StateCoordinatorHandle`
/// (a clone of the mpsc `Sender`) is shared.
struct CoordinatorState {
    snapshot: Snapshot,
    last_settings: Option<Settings>,
    /// Most recent deduped JSONL event slice (from the `ClaudeJsonl` source).
    /// Cached so an OAuth update — which carries the authoritative 5h reset —
    /// can re-derive the window with the correct anchor without the producer
    /// having to re-send the events. `None` until the first JSONL update.
    jsonl_events: Option<Arc<Vec<UsageEvent>>>,
    /// `files_scanned` from the most recent JSONL update, carried alongside
    /// `jsonl_events` so a re-anchor reproduces the same `JsonlSnapshot`.
    files_scanned: usize,
    /// Bundled LiteLLM price table, loaded once at startup. `None` if the
    /// embedded table failed to load (then the cost cell carries an error).
    prices: Option<PriceTable>,
}

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
    pub fn try_send(&self, msg: StateMsg) -> Result<(), Box<mpsc::error::TrySendError<StateMsg>>> {
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

/// Errors that can surface from [`StateCoordinatorHandle::query`].
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The coordinator's tokio task has exited — usually because all handle
    /// clones were dropped (graceful shutdown) or the task panicked.
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
    // Load the bundled LiteLLM price table once for the coordinator's lifetime.
    // The table is embedded and never changes; a load failure (corrupt embed —
    // shouldn't happen on a release build) degrades only the cost cell, not the
    // window.
    let prices = match claude_cost::load_bundled_prices() {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::error!(
                "state_coordinator: bundled price table failed to load ({e}); \
                 anthropic_api_cost will report an error on each JSONL update"
            );
            None
        }
    };
    let mut state = CoordinatorState {
        snapshot: Snapshot::empty(Utc::now()),
        last_settings: None,
        jsonl_events: None,
        files_scanned: 0,
        prices,
    };
    while let Some(msg) = rx.recv().await {
        handle_msg(&mut state, &mut sink, msg);
    }
    tracing::debug!("state_coordinator: channel closed, shutting down");
}

fn handle_msg<S: Sink>(state: &mut CoordinatorState, sink: &mut S, msg: StateMsg) {
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
                let merged_source = partial.source();
                let derived_cost_error = apply_partial(state, partial);
                state.snapshot.fetched_at = Utc::now();
                recompute_pace(state, merged_source);
                sink.on_snapshot(&state.snapshot);
                // The JSONL-derived cost can fail (no price table) inside an
                // otherwise-successful JSONL/OAuth merge. Its error slot is set
                // in the snapshot above; also surface it on the degraded-state
                // channel so sinks emitting `degraded_state` don't miss it.
                if let Some(err) = derived_cost_error {
                    sink.on_degraded(Source::AnthropicApiCost, &err);
                }
            }
            Err(err) => {
                record_error(&mut state.snapshot, source, &err);
                sink.on_degraded(source, &err);
            }
        },
        StateMsg::Query(reply) => {
            // Receiver dropped → caller gave up; nothing to do.
            let _ = reply.send(state.snapshot.clone());
        }
        StateMsg::Refresh => {
            // Re-notify with current state. Sinks that need to repaint will
            // do so; sinks that dedup against `last_painted` will no-op.
            sink.on_snapshot(&state.snapshot);
        }
        StateMsg::SettingsChanged(s) => {
            // Live-apply the provider toggles. The watcher is re-spawned by the
            // host (it decides which pollers run); here we own the Snapshot, so
            // we reset the cell of any now-disabled provider and repaint. A
            // re-enabled provider isn't touched - its poller repopulates it.
            let p = &s.providers;
            if !p.anthropic_enabled {
                clear_source(&mut state.snapshot, Source::ClaudeOAuth);
                state.snapshot.pace.clear();
            }
            // OpenAI keeps polling under a `BALANZE_OPENAI_KEY` env override even
            // with the toggle off, so don't clear a cell that's about to
            // repopulate - mirror the watcher's spawn gate.
            if !p.openai_enabled && !openai_env_key_present() {
                clear_source(&mut state.snapshot, Source::OpenAiCosts);
            }
            if !p.codex_enabled {
                clear_source(&mut state.snapshot, Source::CodexQuota);
            }
            state.last_settings = Some(s);
            sink.on_snapshot(&state.snapshot);
        }
    }
}

/// True if a non-empty `BALANZE_OPENAI_KEY` env override is set. Mirrors the
/// watcher's spawn gate so cell-clearing and polling agree on whether OpenAI is
/// active despite the toggle.
fn openai_env_key_present() -> bool {
    std::env::var("BALANZE_OPENAI_KEY").is_ok_and(|v| !v.trim().is_empty())
}

/// Apply one successful source partial to the snapshot. The coordinator is the
/// sole writer of the in-memory `Snapshot` (AGENTS.md §4 #7), so all mutation
/// is centralized here rather than in a free `merge_partial`.
///
/// Four sources write their cell + clear their own error slot directly.
/// `ClaudeJsonl` is special: it carries raw events, not a finished snapshot, so
/// the coordinator derives the window + cost via [`recompute_jsonl_cells`],
/// anchoring the window to the OAuth 5h reset. `ClaudeOAuth` also triggers a
/// re-derive, because a new reset changes that anchor.
///
/// Returns `Some(err)` if the derived cost failed during this update, so
/// `handle_msg` can fire `Sink::on_degraded(AnthropicApiCost, ..)`.
#[must_use]
fn apply_partial(state: &mut CoordinatorState, partial: SourcePartial) -> Option<String> {
    match partial {
        SourcePartial::ClaudeOAuth(o) => {
            state.snapshot.claude_oauth = Some(o);
            state.snapshot.claude_oauth_error = None;
            // The OAuth feed carries the authoritative 5h reset; re-anchor the
            // cached JSONL window so the live path matches the one-shot CLI.
            recompute_jsonl_cells(state)
        }
        SourcePartial::ClaudeJsonl(input) => {
            state.jsonl_events = Some(input.events);
            state.files_scanned = input.files_scanned;
            // A fresh successful scan clears the JSONL error slot.
            state.snapshot.claude_jsonl_error = None;
            recompute_jsonl_cells(state)
        }
        SourcePartial::CodexQuota(q) => {
            state.snapshot.codex_quota = Some(q);
            state.snapshot.codex_quota_error = None;
            None
        }
        SourcePartial::OpenAiCosts(c) => {
            state.snapshot.openai = Some(c);
            state.snapshot.openai_error = None;
            None
        }
        SourcePartial::ClaudeStatusline(p) => {
            state.snapshot.claude_statusline = Some(p);
            state.snapshot.claude_statusline_error = None;
            None
        }
    }
}

/// Re-derive the two JSONL-fed cells (`claude_jsonl` window + `anthropic_api_cost`)
/// from the cached event slice, anchoring the rolling window to the OAuth 5-hour
/// reset when one is known. Called on every JSONL update (fresh events) AND on
/// every OAuth update (the anchor may have changed) — this is the fix for the
/// CLI≢watcher window divergence: both paths now run `summarize_jsonl` with the
/// same anchor. No-op until the first JSONL events arrive.
///
/// Does NOT touch `claude_jsonl_error` (owned by the JSONL update path). It does
/// own `anthropic_api_cost_error`, since that error is purely a function of
/// price-table availability, which it evaluates here.
///
/// Returns `Some(err)` when the derived cost failed (no price table). The caller
/// (`handle_msg`) surfaces it through `Sink::on_degraded` in addition to setting
/// the snapshot's `anthropic_api_cost_error` slot — so sinks that emit the
/// `degraded_state` event (the Tauri UI) don't miss a cost degradation now
/// that the cost is derived here rather than arriving as its own `Err` update.
#[must_use]
fn recompute_jsonl_cells(state: &mut CoordinatorState) -> Option<String> {
    // Cheap Arc clone to drop the borrow on `state.jsonl_events` before we take
    // `&mut state.snapshot` below.
    let events = state.jsonl_events.clone()?;
    let anchor = state
        .snapshot
        .claude_oauth
        .as_ref()
        .and_then(ClaudeOAuthSnapshot::five_hour_reset);
    let cells = summarize_jsonl(
        &events,
        Utc::now(),
        state.files_scanned,
        anchor,
        state.prices.as_ref(),
    );
    state.snapshot.claude_jsonl = Some(cells.jsonl);
    match cells.cost {
        Ok(c) => {
            state.snapshot.anthropic_api_cost = Some(c);
            state.snapshot.anthropic_api_cost_error = None;
            None
        }
        // Keep any prior cost data visible (stale-with-indicator); set the error
        // slot AND return it so the caller can fire `on_degraded`.
        Err(e) => {
            state.snapshot.anthropic_api_cost_error = Some(e.clone());
            Some(e)
        }
    }
}

/// Recompute `snapshot.pace` from the current OAuth cadence bars. Only an OAuth
/// merge changes the cadence data, so other sources are a no-op.
///
// TODO: also derive pace from the statusline feed. During an active Claude Code
// session the statusline payload carries fresh `rate_limits` and is the live
// backbone (OAuth is the backoff'd, 429-prone fallback), so OAuth-only pace can
// go stale or empty exactly when the user is most active. Not yet wired.
fn recompute_pace(state: &mut CoordinatorState, merged_source: Source) {
    if merged_source != Source::ClaudeOAuth {
        return;
    }
    state.snapshot.pace = state
        .snapshot
        .claude_oauth
        .as_ref()
        .map(|o| pace_for_oauth(o, Utc::now()))
        .unwrap_or_default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{ClaudeJsonlInput, Source, SourcePartial, SourceUpdate};
    use crate::sink::{NullSink, Sink};
    use crate::snapshot::Snapshot;
    use crate::test_support::{
        oauth_snapshot, oauth_snapshot_with_reset, openai_costs, sample_events,
    };
    use chrono::Duration;
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
        assert_eq!(snap.openai_error.as_deref(), Some("network unreachable"));
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
        // Seed the snapshot with one JSONL update (raw events; the coordinator
        // derives the window + cost cells).
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                    events: Arc::new(sample_events()),
                    files_scanned: 5,
                })),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        assert!(snap.claude_jsonl.is_some());
        assert_eq!(snap.claude_jsonl.as_ref().unwrap().files_scanned, 5);
    }

    #[tokio::test]
    async fn jsonl_update_derives_window_and_cost_cells() {
        let (handle, _join) = spawn(NullSink);
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                    events: Arc::new(sample_events()),
                    files_scanned: 2,
                })),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        // The coordinator derives BOTH JSONL-fed cells from the raw events.
        assert!(snap.claude_jsonl.is_some(), "window cell derived");
        assert!(snap.claude_jsonl_error.is_none());
        let cost = snap
            .anthropic_api_cost
            .expect("cost cell derived from the same events");
        assert!(
            cost.total_micro_usd > 0,
            "sample models are in the bundled price table"
        );
        assert!(snap.anthropic_api_cost_error.is_none());
    }

    #[tokio::test]
    async fn oauth_update_reanchors_cached_jsonl_window() {
        // The WS1 invariant: the window the watcher path produces must anchor
        // to the OAuth 5h reset (parity with the one-shot CLI), even though
        // JSONL events arrive in a separate message BEFORE the reset is known.
        // A regression here reintroduces the CLI≢watcher divergence.
        let (handle, _join) = spawn(NullSink);

        // 1) JSONL events arrive first — no OAuth yet ⇒ now-relative window.
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                    events: Arc::new(sample_events()),
                    files_scanned: 1,
                })),
            }))
            .await
            .unwrap();

        // 2) OAuth arrives with a strictly-future 5h reset.
        let reset = Utc::now() + Duration::minutes(90);
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeOAuth,
                result: Ok(SourcePartial::ClaudeOAuth(oauth_snapshot_with_reset(reset))),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        let window = snap.claude_jsonl.expect("jsonl cell present").window;
        // Re-anchored: window_start is reset - 5h, NOT now - 5h.
        assert_eq!(
            window.window_start,
            reset - window::DEFAULT_WINDOW,
            "OAuth update must re-anchor the cached JSONL window to reset - 5h"
        );
    }

    #[tokio::test]
    async fn successful_update_clears_only_its_own_error() {
        // Cross-source isolation: a successful merge clears ONLY the merged
        // source's error slot. Migrated from snapshot.rs now that snapshot
        // mutation lives in the coordinator's `apply_partial`.
        let (handle, _join) = spawn(NullSink);

        // Seed two independent errors.
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::OpenAiCosts,
                result: Err("openai 500".to_string()),
            }))
            .await
            .unwrap();
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Err("jsonl perm denied".to_string()),
            }))
            .await
            .unwrap();

        // A successful JSONL update clears its own error and leaves OpenAI's.
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                    events: Arc::new(sample_events()),
                    files_scanned: 1,
                })),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        assert!(snap.claude_jsonl.is_some());
        assert!(
            snap.claude_jsonl_error.is_none(),
            "merged source's own error is cleared"
        );
        assert_eq!(
            snap.openai_error.as_deref(),
            Some("openai 500"),
            "an unrelated source's error must be left untouched"
        );
    }

    #[test]
    fn cost_failure_sets_error_and_is_returned_for_on_degraded() {
        // When the bundled price table is unavailable, the JSONL-derived cost
        // fails. `recompute_jsonl_cells` must set `anthropic_api_cost_error` AND
        // return the error so `handle_msg` fires `on_degraded` — the cost
        // degradation must not be silently buried in the snapshot (pre-refactor
        // it arrived as its own `Err` update that reached `on_degraded`).
        let mut state = CoordinatorState {
            snapshot: Snapshot::empty(Utc::now()),
            last_settings: None,
            jsonl_events: Some(Arc::new(sample_events())),
            files_scanned: 1,
            prices: None, // bundled table "unavailable"
        };

        let returned = recompute_jsonl_cells(&mut state);

        assert!(
            returned.is_some(),
            "cost failure must be returned so the caller can fire on_degraded"
        );
        assert!(
            state.snapshot.anthropic_api_cost_error.is_some(),
            "the error slot is also set on the snapshot"
        );
        assert!(
            state.snapshot.claude_jsonl.is_some(),
            "the window cell is still produced despite the cost failure"
        );
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
    async fn settings_change_clears_disabled_provider_cells() {
        let (handle, _join) = spawn(NullSink);
        // Seed OAuth (also populates `pace`) and OpenAI costs.
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeOAuth,
                result: Ok(SourcePartial::ClaudeOAuth(oauth_snapshot())),
            }))
            .await
            .unwrap();
        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::OpenAiCosts,
                result: Ok(SourcePartial::OpenAiCosts(openai_costs())),
            }))
            .await
            .unwrap();

        let before = handle.query().await.unwrap();
        assert!(before.claude_oauth.is_some());
        assert!(before.openai.is_some());
        assert!(!before.pace.is_empty(), "oauth seeded pace");

        // Disable Anthropic (clears its cell + pace); keep OpenAI enabled so its
        // cell is preserved - proving per-provider isolation of the clear.
        let s = Settings {
            providers: settings::ProviderSettings {
                anthropic_enabled: false,
                openai_enabled: true,
                codex_enabled: true,
            },
            ..Settings::default()
        };
        handle.send(StateMsg::SettingsChanged(s)).await.unwrap();

        let after = handle.query().await.unwrap();
        assert!(
            after.claude_oauth.is_none(),
            "disabled Anthropic cell must be cleared"
        );
        assert!(after.pace.is_empty(), "pace cleared alongside oauth");
        assert!(
            after.openai.is_some(),
            "still-enabled OpenAI cell preserved"
        );
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
            openai.total_micro_usd = i as i64;
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
            snap.openai.as_ref().unwrap().total_micro_usd,
            (N - 1) as i64,
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

    #[tokio::test]
    async fn oauth_merge_populates_snapshot_pace() {
        // An OAuth update carrying a five_hour cadence must produce a non-empty
        // `pace` vec with a `five_hour` entry with plausible fractions in [0, 1].
        let (handle, _join) = spawn(NullSink);

        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeOAuth,
                result: Ok(SourcePartial::ClaudeOAuth(oauth_snapshot())),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        assert!(
            !snap.pace.is_empty(),
            "OAuth merge with cadence data must populate snap.pace"
        );
        let five = snap
            .pace
            .iter()
            .find(|wp| wp.key == "five_hour")
            .expect("five_hour entry must be present after OAuth merge");
        assert!(
            (0.0..=1.0).contains(&five.used_fraction),
            "used_fraction must be in [0, 1]; got {}",
            five.used_fraction
        );
        assert!(
            (0.0..=1.0).contains(&five.elapsed_fraction),
            "elapsed_fraction must be in [0, 1]; got {}",
            five.elapsed_fraction
        );
    }

    #[tokio::test]
    async fn jsonl_only_update_does_not_populate_pace() {
        // `recompute_pace` is guarded to OAuth merges. A JSONL-only update must
        // leave `pace` empty (no OAuth cadence data is available to derive from).
        let (handle, _join) = spawn(NullSink);

        handle
            .send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                    events: Arc::new(sample_events()),
                    files_scanned: 3,
                })),
            }))
            .await
            .unwrap();

        let snap = handle.query().await.unwrap();
        assert!(
            snap.pace.is_empty(),
            "a JSONL-only update must not populate pace (no OAuth cadence to derive from); \
             got {:?}",
            snap.pace
        );
    }
}
