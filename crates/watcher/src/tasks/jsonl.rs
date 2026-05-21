//! JSONL notify task. Watches `<claude_home>/projects/**/*.jsonl` via
//! `notify::recommended_watcher`, debounces bursts for 300ms, then
//! re-walks + parses on each batch (full rescan for now;
//! `IncrementalParser` byte-cursor optimization is a follow-up if
//! cold-start latency bites — TODO(v0.2-followup): replace the full
//! rescan with `IncrementalParser` byte-cursor reads to reduce I/O on
//! large `~/.claude/` trees during active Claude Code sessions).

use std::path::Path;
use std::time::Duration;

use claude_cost::{compute_cost, load_bundled_prices};
use claude_parser::{
    dedup_events, find_claude_projects_dir, find_jsonl_files, parse_str, UsageEvent,
};
use notify::{RecursiveMode, Watcher as _};
use state_coordinator::{
    JsonlSnapshot, Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg,
};
use tokio::sync::mpsc;
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

        // notify callbacks fire on a background thread; bridge to our
        // tokio task via an unbounded channel (channel closed = task exit).
        let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
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

        if watcher
            .watch(&projects_dir, RecursiveMode::Recursive)
            .is_err()
        {
            tracing::error!(
                "watcher/jsonl: failed to watch {}; reporting NotifyExhausted",
                projects_dir.display()
            );
            return Err(WatcherError::NotifyExhausted {
                affected: Source::ClaudeJsonl,
            });
        }

        // Initial scan — there may already be JSONL files on disk when the
        // watcher starts (e.g. on app launch, the user already has sessions).
        emit_jsonl_snapshot(&coord, &projects_dir).await;

        let mut pending = false;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(DEBOUNCE), if pending => {
                    pending = false;
                    emit_jsonl_snapshot(&coord, &projects_dir).await;
                }
                ev = rx.recv() => match ev {
                    Some(Ok(_)) => { pending = true; }
                    Some(Err(e)) => tracing::warn!("watcher/jsonl: notify error: {e}"),
                    None => return Ok(()), // tx dropped → watcher destroyed → exit cleanly
                },
            }
        }
    })
}

/// Re-scan the projects directory, synthesize a `JsonlSnapshot` and
/// (if price table loads) an `AnthropicApiCost`, and send both to the
/// coordinator. Errors from individual JSONL files are logged at `warn!`
/// and skipped (mirrors `live_load_claude_events` in `balanze_cli`).
async fn emit_jsonl_snapshot(coord: &StateCoordinatorHandle, projects_dir: &Path) {
    let files = match find_jsonl_files(projects_dir) {
        Ok(f) => f,
        Err(e) => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Err(format!("find_jsonl_files: {e}")),
                }))
                .await;
            return;
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
    let _ = coord
        .send(StateMsg::Update(SourceUpdate {
            source: Source::ClaudeJsonl,
            result: Ok(SourcePartial::ClaudeJsonl(jsonl)),
        }))
        .await;

    // Cost emit is separate so a price-load failure doesn't suppress the
    // JSONL window snapshot (the two cells are independent in the UI).
    match load_bundled_prices() {
        Ok(prices) => {
            let cost = compute_cost(&all_events, &prices);
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::AnthropicApiCost,
                    result: Ok(SourcePartial::AnthropicApiCost(cost)),
                }))
                .await;
        }
        Err(e) => {
            tracing::warn!("watcher/jsonl: price table load failed: {e}; emitting cost error");
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::AnthropicApiCost,
                    result: Err(format!("price table: {e}")),
                }))
                .await;
        }
    }
}
