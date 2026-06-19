//! JSONL notify task. Watches `<claude_home>/projects/**/*.jsonl` via
//! `notify::recommended_watcher`, debounces bursts for 300ms, then reads only
//! the newly-appended bytes of each changed file via a per-file byte cursor
//! (`claude_parser::IncrementalParser`, AGENTS.md §3.1) and emits the running
//! deduped event set. A 60s fallback tick catches filesystem events `notify`
//! drops (inotify exhaustion, atomic-rewrite detection lag) - also incremental,
//! so it is NOT a periodic full reparse. The only full read is the first read
//! of each file at launch.
//!
//! This task owns the sole `IncrementalParser` for JSONL; the safety poll no
//! longer re-scans JSONL (the 60s fallback here replaces that leg), so there is
//! exactly one byte-cursor set and one emitter for the `ClaudeJsonl` source.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use claude_parser::{
    IncrementalParser, ParseError, UsageEvent, dedup_events, find_all_claude_projects_dirs,
    find_jsonl_files,
};
use notify::{RecursiveMode, Watcher as _};
use state_coordinator::{
    ClaudeJsonlInput, Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

/// How long we wait after the last notify event before re-scanning. Debouncing
/// collapses burst writes (e.g. a long Claude response that flushes many small
/// writes before the session file is closed) into a single incremental read.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Fallback re-scan cadence. A safety net for events `notify` misses; reads
/// incrementally (only new bytes), so it costs nothing like a full reparse.
const FALLBACK_POLL: Duration = Duration::from_secs(60);

/// Per-task incremental scan state: the byte-cursor parser plus the running
/// deduped event set. Threaded through `spawn_blocking` (moved in, moved back
/// out) so the blocking file I/O never runs on a tokio worker (AGENTS.md §2.1).
struct ScanState {
    parser: IncrementalParser,
    accumulated: Vec<UsageEvent>,
}

impl ScanState {
    fn new() -> Self {
        Self {
            parser: IncrementalParser::new(),
            accumulated: Vec::new(),
        }
    }
}

/// Spawn the JSONL notify task and return its `JoinHandle`.
///
/// The task:
/// 1. Resolves ALL existing Claude projects directories via
///    `find_all_claude_projects_dirs()` (a dual-install machine can have both
///    `~/.claude/projects` and `~/.config/claude/projects`). If none, logs at
///    `warn!` and exits `Ok(())` - not all users have Claude Code installed.
/// 2. Sets up a `notify::recommended_watcher` watching EACH root.
///    If that fails (OS resource exhausted), returns
///    `Err(WatcherError::NotifyExhausted)`.
/// 3. Emits an initial snapshot immediately (reads existing JSONL files in full
///    once, establishing the byte cursors).
/// 4. On each debounced notify batch OR 60s fallback tick, reads new bytes only
///    and re-emits the accumulated deduped events.
///
/// The task exits `Ok(())` when the notify channel closes (tx dropped).
pub(crate) fn spawn(coord: StateCoordinatorHandle) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let roots = find_all_claude_projects_dirs();
        if roots.is_empty() {
            tracing::warn!("watcher/jsonl: no Claude projects dir found; task exits clean");
            return Ok(());
        }

        // `Notify` coalesces "something changed" signals without queuing -
        // multiple notify_one() calls before notified().await fires collapse
        // into a single wake-up. Earlier design used an unbounded mpsc which
        // could grow without bound under bursty filesystem activity; this
        // version uses no queue at all and relies on the 300ms DEBOUNCE for
        // batching. Notify errors are logged inside the callback itself.
        let signal = Arc::new(Notify::new());
        let signal_cb = signal.clone();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Err(e) = &res {
                    tracing::warn!("watcher/jsonl: notify error: {e}");
                }
                // Wake the task regardless of Ok/Err - on errors we re-scan
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

        for root in &roots {
            if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
                tracing::error!(
                    "watcher/jsonl: failed to watch {} ({e}); reporting NotifyExhausted",
                    root.display()
                );
                return Err(WatcherError::NotifyExhausted {
                    affected: Source::ClaudeJsonl,
                });
            }
        }

        // 60s fallback ticker. `Delay` (not the default `Burst`) so a long scan
        // can't queue multiple missed ticks and fire back-to-back on recovery.
        let mut fallback = tokio::time::interval(FALLBACK_POLL);
        fallback.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Consume the immediate first tick - the initial scan below covers it.
        fallback.tick().await;

        // Initial scan - the first `read_incremental` of each existing file
        // reads it in full (the only full read; AGENTS.md §3.1 "launch"), and
        // sets the cursor so later reads pick up appends only. Note that
        // `watcher.watch(...)` above registers atomically with the OS; writes
        // after it returns are buffered by the OS and drained by the callback.
        let mut state = ScanState::new();
        state = emit_incremental(&coord, &roots, state).await;

        loop {
            tokio::select! {
                _ = signal.notified() => {
                    // Debounce the burst - further notify_one() calls during
                    // this sleep coalesce into the next iteration (Notify keeps
                    // at most one pending permit).
                    tokio::time::sleep(DEBOUNCE).await;
                }
                _ = fallback.tick() => {}
            }
            state = emit_incremental(&coord, &roots, state).await;
        }
    })
}

/// Result of the blocking incremental scan. Carries the deduped events the
/// async caller forwards to the coordinator, which derives the window summary
/// and the API-rate cost from them (anchoring the window to the OAuth 5h reset).
enum ScanResult {
    /// Successful walk + incremental read + dedup.
    Ok {
        events: Vec<UsageEvent>,
        files_scanned: usize,
    },
    /// `find_jsonl_files` itself failed on every root - the scan never started.
    Fatal { error: String },
}

/// Run the incremental scan off the tokio runtime, then emit the result to the
/// coordinator. `state` is moved into the blocking task and moved back out so
/// the byte cursors + accumulated events persist across calls. On a panic the
/// state is lost and a fresh one is returned - the next scan then re-reads
/// every file from byte 0 (the launch path), which is the safe recovery.
async fn emit_incremental(
    coord: &StateCoordinatorHandle,
    roots: &[PathBuf],
    state: ScanState,
) -> ScanState {
    let roots_owned: Vec<PathBuf> = roots.to_vec();

    let joined = tokio::task::spawn_blocking(move || {
        let mut state = state;
        let scan = scan_incremental(&roots_owned, &mut state);
        (state, scan)
    })
    .await;

    match joined {
        Ok((
            state,
            ScanResult::Ok {
                events,
                files_scanned,
            },
        )) => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                        events: Arc::new(events),
                        files_scanned,
                    })),
                }))
                .await;
            state
        }
        Ok((state, ScanResult::Fatal { error })) => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Err(error),
                }))
                .await;
            state
        }
        Err(join_err) => {
            tracing::error!("watcher/jsonl: scan task panicked: {join_err}");
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeJsonl,
                    result: Err(format!("scan task panicked: {join_err}")),
                }))
                .await;
            ScanState::new()
        }
    }
}

/// Walk all roots, incrementally read newly-appended bytes from each file via
/// the per-file byte cursor, accumulate into the running deduped event set, and
/// return it. Only NEW bytes are read + parsed after a file's first scan, so
/// this stays flat-CPU during active Claude Code sessions (AGENTS.md §3.1).
/// Runs on a blocking worker via `spawn_blocking`; does NOT touch the runtime.
///
/// Per-root walk failures are logged + skipped so one bad root doesn't lose the
/// others. `Fatal` is returned only when NO files were collected from any root
/// AND at least one root failed - so an unreadable root can't masquerade as an
/// empty-but-fine window. Per-file read errors:
/// - `FileMissing` (the file vanished between walk and read): drop its cursor so
///   a re-created file is re-read from byte 0; skip it this round.
/// - any other parse / IO error: log + leave the cursor parked. The good prefix
///   already parsed stays accumulated, and only the small un-parsed tail is
///   retried next time - never a full re-read.
///
/// Accumulation note: `read_incremental` returns per-file deltas, so we keep a
/// running `accumulated` set and `dedup_events` it each round. Claude Code only
/// appends to JSONL, so a file's events arrive once; the dedup collapses the
/// rare same-event-twice (a defensive cursor reset, or a session mirrored under
/// two roots). Events from a deleted file linger in `accumulated` (Claude does
/// not delete sessions), and old events are not pruned - both match the prior
/// full-scan behavior, which also held every event from every file.
fn scan_incremental(roots: &[PathBuf], state: &mut ScanState) -> ScanResult {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut walk_err: Option<String> = None;
    for root in roots {
        match find_jsonl_files(root) {
            Ok(mut f) => files.append(&mut f),
            Err(e) => {
                let msg = format!("find_jsonl_files {}: {e}", root.display());
                tracing::warn!("watcher/jsonl: skipping root ({msg})");
                walk_err.get_or_insert(msg);
            }
        }
    }
    if files.is_empty() {
        if let Some(error) = walk_err {
            return ScanResult::Fatal { error };
        }
    }

    let files_scanned = files.len();
    for path in &files {
        match state.parser.read_incremental(path) {
            Ok(new_events) => state.accumulated.extend(new_events),
            Err(ParseError::FileMissing(_)) => {
                // Vanished between walk and read; forget the cursor so a
                // re-created file at this path is re-read from byte 0.
                state.parser.invalidate(path);
            }
            Err(e) => {
                tracing::warn!(
                    "watcher/jsonl: incremental read error in {} ({e})",
                    path.display()
                );
            }
        }
    }
    dedup_events(&mut state.accumulated);

    ScanResult::Ok {
        events: state.accumulated.clone(),
        files_scanned,
    }
}
