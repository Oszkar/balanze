//! JSONL notify task. Watches `<claude_home>/projects/**/*.jsonl` via
//! `notify::recommended_watcher`, debounces bursts for 300ms, then
//! re-walks + parses on each batch (full rescan for now;
//! `IncrementalParser` byte-cursor optimization is a follow-up if
//! cold-start latency bites — TODO(v0.2-followup): replace the full
//! rescan with `IncrementalParser` byte-cursor reads to reduce I/O on
//! large `~/.claude/` trees during active Claude Code sessions).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use claude_cost::{compute_cost, load_bundled_prices, Cost, PriceTable};
use claude_parser::{
    dedup_events, find_claude_projects_dir, find_jsonl_files, parse_str, UsageEvent,
};
use notify::{RecursiveMode, Watcher as _};
use state_coordinator::{
    JsonlSnapshot, Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use window::{summarize_window, DEFAULT_BURN_WINDOW, DEFAULT_MIN_BURN_EVENTS, DEFAULT_WINDOW};

use crate::errors::WatcherError;

/// How long we wait after the last notify event before re-scanning. Debouncing
/// prevents redundant full re-parses during burst writes (e.g. a long Claude
/// response that flushes many small writes before the session file is closed).
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Spawn the JSONL notify task and return its `JoinHandle`.
///
/// The task:
/// 1. Resolves the Claude projects directory via `find_claude_projects_dir()`.
///    If absent, logs at `warn!` and exits `Ok(())` — not all users have
///    Claude Code installed.
/// 2. Sets up a `notify::recommended_watcher` on the projects directory.
///    If that fails (OS resource exhausted), returns
///    `Err(WatcherError::NotifyExhausted)`.
/// 3. Emits an initial snapshot immediately (picks up existing JSONL files).
/// 4. On each debounced batch of notify events, re-scans and re-emits.
///
/// The task exits `Ok(())` when the notify channel closes (tx dropped).
pub(crate) fn spawn(coord: StateCoordinatorHandle) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let projects_dir = match find_claude_projects_dir() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "watcher/jsonl: no Claude projects dir found ({e}); task exits clean"
                );
                return Ok(());
            }
        };

        // Load the bundled LiteLLM price table ONCE for the task lifetime.
        // It's embedded JSON of fixed-known shape (no I/O on rescan, no
        // network) and never changes between scans, so reparsing it per
        // burst is pure waste. If the bundled file itself is corrupt we
        // log + still emit JSONL snapshots (cost emits become per-burst
        // errors, which the coordinator records as `anthropic_api_cost_error`).
        let prices: Option<Arc<PriceTable>> = match load_bundled_prices() {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                tracing::error!(
                    "watcher/jsonl: bundled price table load failed once at task \
                     start ({e}); will emit AnthropicApiCost errors per rescan"
                );
                None
            }
        };

        // `Notify` coalesces "something changed" signals without queuing —
        // multiple notify_one() calls before notified().await fires
        // collapse into a single wake-up. Earlier design used an unbounded
        // mpsc which could grow without bound under bursty filesystem
        // activity; this version uses no queue at all and relies on the
        // 300ms DEBOUNCE for batching. Notify errors are logged inside
        // the callback itself (no longer carried through the channel) so
        // we keep visibility without retaining the event payload.
        let signal = Arc::new(Notify::new());
        let signal_cb = signal.clone();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Err(e) = &res {
                    tracing::warn!("watcher/jsonl: notify error: {e}");
                }
                // Wake the task regardless of Ok/Err — on errors we re-scan
                // to verify state; DEBOUNCE in the loop gates retry frequency
                // so a stream of errors can't burn CPU.
                signal_cb.notify_one();
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!(
                        "watcher/jsonl: notify init failed ({e}); reporting NotifyExhausted"
                    );
                    return Err(WatcherError::NotifyExhausted {
                        affected: Source::ClaudeJsonl,
                    });
                }
            };

        if let Err(e) = watcher.watch(&projects_dir, RecursiveMode::Recursive) {
            tracing::error!(
                "watcher/jsonl: failed to watch {} ({e}); reporting NotifyExhausted",
                projects_dir.display()
            );
            return Err(WatcherError::NotifyExhausted {
                affected: Source::ClaudeJsonl,
            });
        }

        // Initial scan — picks up files that already exist when the watcher
        // starts (e.g. existing Claude Code sessions on app launch). Note
        // that `watcher.watch(...)` above registers atomically with the OS;
        // any writes after that call returns are buffered by the OS (inotify
        // on Linux / ReadDirectoryChangesW on Windows / FSEvents on macOS)
        // and drained by the notify callback below. So the only events the
        // OS does NOT deliver are those that landed before `watch()`
        // returned — this scan covers exactly that window.
        emit_jsonl_snapshot(&coord, &projects_dir, prices.as_deref()).await;

        loop {
            // Wait for at least one notify event.
            signal.notified().await;
            // Debounce the burst — any further notify_one() calls during
            // this sleep coalesce into the next iteration's notified()
            // (Notify accumulates at most one pending permit).
            tokio::time::sleep(DEBOUNCE).await;
            emit_jsonl_snapshot(&coord, &projects_dir, prices.as_deref()).await;
        }
    })
}

/// Result of the blocking scan + compute step. Carries everything the
/// async caller needs to send to the coordinator; constructed off the
/// tokio runtime by `scan_and_compute`.
pub(crate) enum ScanResult {
    /// Successful walk + parse. Includes both cells (cost may be `Err`
    /// if no prices were available at task start).
    Ok {
        jsonl: JsonlSnapshot,
        cost: Result<Cost, String>,
    },
    /// `find_jsonl_files` itself failed — the rescan never even started.
    Fatal { error: String },
}

/// Re-scan the projects directory, synthesize a `JsonlSnapshot` and
/// (if a price table was loaded at task start) an `AnthropicApiCost`,
/// and send both to the coordinator.
///
/// The sync I/O (`find_jsonl_files` + per-file `read_to_string` + parse +
/// dedup) is wrapped in `tokio::task::spawn_blocking` so it does not
/// block a tokio worker. The coordinator sends stay on the async side.
async fn emit_jsonl_snapshot(
    coord: &StateCoordinatorHandle,
    projects_dir: &Path,
    prices: Option<&PriceTable>,
) {
    let projects_dir_owned: PathBuf = projects_dir.to_path_buf();
    // `compute_cost` takes `&PriceTable`; the blocking task moves owned
    // data only, so clone the table into a fresh Arc-owned copy for
    // moving. Cloning is cheap (the table is a small embedded JSON
    // structure) and bounded by the bundled file size.
    let prices_owned: Option<PriceTable> = prices.cloned();

    let result = tokio::task::spawn_blocking(move || {
        scan_and_compute(&projects_dir_owned, prices_owned.as_ref())
    })
    .await;

    let scan = match result {
        Ok(r) => r,
        Err(join_err) => {
            // `spawn_blocking` task panicked. Treat as a transient parse
            // error; the next notify event will trigger another scan.
            tracing::error!("watcher/jsonl: scan task panicked: {join_err}");
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Err(format!("scan task panicked: {join_err}")),
                }))
                .await;
            return;
        }
    };

    match scan {
        ScanResult::Ok { jsonl, cost } => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Ok(SourcePartial::ClaudeJsonl(jsonl)),
                }))
                .await;

            // Cost emit is separate so a price-load failure doesn't suppress
            // the JSONL window snapshot (the two cells are independent in
            // the UI).
            match cost {
                Ok(c) => {
                    let _ = coord
                        .send(StateMsg::Update(SourceUpdate {
                            source: Source::AnthropicApiCost,
                            result: Ok(SourcePartial::AnthropicApiCost(c)),
                        }))
                        .await;
                }
                Err(msg) => {
                    let _ = coord
                        .send(StateMsg::Update(SourceUpdate {
                            source: Source::AnthropicApiCost,
                            result: Err(msg),
                        }))
                        .await;
                }
            }
        }
        ScanResult::Fatal { error } => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Err(error),
                }))
                .await;
        }
    }
}

/// Synchronous scan + parse + compute. Runs on a blocking worker via
/// `spawn_blocking`; does NOT touch the tokio runtime. Errors from
/// individual JSONL files are logged at `warn!` and skipped (mirrors
/// `live_load_claude_events` in `balanze_cli`).
///
/// `pub(crate)` so the safety-poll task can reuse it without duplicating
/// the walk-parse-summarize-cost pipeline.
pub(crate) fn scan_and_compute(projects_dir: &Path, prices: Option<&PriceTable>) -> ScanResult {
    let files = match find_jsonl_files(projects_dir) {
        Ok(f) => f,
        Err(e) => {
            return ScanResult::Fatal {
                error: format!("find_jsonl_files: {e}"),
            };
        }
    };

    let files_scanned = files.len();
    let mut all_events: Vec<UsageEvent> = Vec::new();
    for path in &files {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("watcher/jsonl: skipping {} ({e})", path.display());
                continue;
            }
        };
        match parse_str(&content) {
            Ok(events) => all_events.extend(events),
            Err(e) => {
                tracing::warn!("watcher/jsonl: parse error in {} ({e})", path.display());
            }
        }
    }
    dedup_events(&mut all_events);

    let now = chrono::Utc::now();
    let summary = summarize_window(
        &all_events,
        now,
        DEFAULT_WINDOW,
        DEFAULT_BURN_WINDOW,
        DEFAULT_MIN_BURN_EVENTS,
        None, // window_anchor — OAuth-anchored math is 5b territory
    );

    let jsonl = JsonlSnapshot {
        files_scanned,
        window: summary,
    };
    let cost = match prices {
        Some(p) => Ok(compute_cost(&all_events, p)),
        None => Err("price table unavailable (load failed at task start)".to_string()),
    };

    ScanResult::Ok { jsonl, cost }
}
