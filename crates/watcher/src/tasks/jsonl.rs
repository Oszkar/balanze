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

use claude_parser::{
    dedup_events, find_claude_projects_dir, find_jsonl_files, parse_str, UsageEvent,
};
use notify::{RecursiveMode, Watcher as _};
use state_coordinator::{
    ClaudeJsonlInput, Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

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
        emit_jsonl_snapshot(&coord, &projects_dir).await;

        loop {
            // Wait for at least one notify event.
            signal.notified().await;
            // Debounce the burst — any further notify_one() calls during
            // this sleep coalesce into the next iteration's notified()
            // (Notify accumulates at most one pending permit).
            tokio::time::sleep(DEBOUNCE).await;
            emit_jsonl_snapshot(&coord, &projects_dir).await;
        }
    })
}

/// Result of the blocking walk + parse step. Carries the deduped events the
/// async caller forwards to the coordinator, which derives the window summary
/// and the API-rate cost from them (anchoring the window to the OAuth 5h
/// reset). Constructed off the tokio runtime by `scan_events`.
pub(crate) enum ScanResult {
    /// Successful walk + parse + dedup.
    Ok {
        events: Vec<UsageEvent>,
        files_scanned: usize,
    },
    /// `find_jsonl_files` itself failed — the rescan never even started.
    Fatal { error: String },
}

/// Re-scan the projects directory and send the deduped events to the
/// coordinator as a `ClaudeJsonl` update. The coordinator derives BOTH the
/// window summary and the API-rate cost from these events (anchoring the
/// window to the OAuth 5h reset), so this task no longer computes them — that
/// keeps the live path identical to the one-shot CLI (AGENTS.md §4 #8).
///
/// The sync I/O (`find_jsonl_files` + per-file `read_to_string` + parse +
/// dedup) is wrapped in `tokio::task::spawn_blocking` so it does not block a
/// tokio worker. The coordinator send stays on the async side.
async fn emit_jsonl_snapshot(coord: &StateCoordinatorHandle, projects_dir: &Path) {
    let projects_dir_owned: PathBuf = projects_dir.to_path_buf();

    let result = tokio::task::spawn_blocking(move || scan_events(&projects_dir_owned)).await;

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
        ScanResult::Ok {
            events,
            files_scanned,
        } => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                        events: Arc::new(events),
                        files_scanned,
                    })),
                }))
                .await;
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

/// Synchronous walk + parse + dedup. Runs on a blocking worker via
/// `spawn_blocking`; does NOT touch the tokio runtime. Errors from individual
/// JSONL files are logged at `warn!` and skipped (mirrors
/// `live_load_claude_events` in `balanze_cli`).
///
/// `pub(crate)` so the safety-poll task can reuse it without duplicating the
/// walk-parse-dedup pipeline. The window + cost synthesis lives in the
/// coordinator (`state_coordinator::summarize_jsonl`), shared with the CLI, so
/// the OAuth-anchored window is computed identically on both paths.
pub(crate) fn scan_events(projects_dir: &Path) -> ScanResult {
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

    ScanResult::Ok {
        events: all_events,
        files_scanned,
    }
}
