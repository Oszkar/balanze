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

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use claude_parser::{
    IncrementalParser, IncrementalRead, ParseError, UsageEvent, dedup_events,
    find_all_claude_projects_dirs, find_jsonl_files,
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

/// Per-task incremental scan state: the byte-cursor parser plus each file's
/// owned event contribution. Threaded through `spawn_blocking` (moved in,
/// moved back out) so blocking file I/O never runs on a tokio worker.
struct ScanState {
    parser: IncrementalParser,
    by_file: BTreeMap<PathBuf, Vec<UsageEvent>>,
}

impl ScanState {
    fn new() -> Self {
        Self {
            parser: IncrementalParser::new(),
            by_file: BTreeMap::new(),
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
/// the byte cursors + per-file events persist across calls. On a panic the
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

/// Walk all roots, incrementally update each file's owned event contribution,
/// then flatten and deduplicate across files. Appends extend one contribution;
/// truncations and rewrites replace it so vanished events cannot linger.
/// Runs on a blocking worker via `spawn_blocking`; does NOT touch the runtime.
///
/// Per-root walk failures are logged + skipped so one bad root doesn't lose the
/// others. `Fatal` is returned only when NO files were collected from any root
/// AND at least one root failed - so an unreadable root can't masquerade as an
/// empty-but-fine window. Per-file read errors:
/// - `FileMissing` removes the vanished file's cursor and contribution.
/// - any other parse / IO error logs and retains the last good contribution.
fn scan_incremental(roots: &[PathBuf], state: &mut ScanState) -> ScanResult {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut successful_roots: Vec<&PathBuf> = Vec::new();
    let mut walk_err: Option<String> = None;
    for root in roots {
        match find_jsonl_files(root) {
            Ok(mut f) => {
                successful_roots.push(root);
                files.append(&mut f);
            }
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
    let discovered: HashSet<&PathBuf> = files.iter().collect();
    let removed: Vec<PathBuf> = state
        .by_file
        .keys()
        .filter(|path| {
            successful_roots.iter().any(|root| path.starts_with(root)) && !discovered.contains(path)
        })
        .cloned()
        .collect();
    for path in removed {
        state.by_file.remove(&path);
        state.parser.invalidate(&path);
    }

    for path in &files {
        match state.parser.read_incremental(path) {
            Ok(IncrementalRead::Append(new_events)) => {
                state
                    .by_file
                    .entry(path.clone())
                    .or_default()
                    .extend(new_events);
            }
            Ok(IncrementalRead::Replace(events)) => {
                state.by_file.insert(path.clone(), events);
            }
            Err(ParseError::FileMissing(_)) => {
                state.parser.invalidate(path);
                state.by_file.remove(path);
            }
            Err(e) => {
                tracing::warn!(
                    "watcher/jsonl: incremental read error in {} ({e})",
                    path.display()
                );
            }
        }
    }
    // Preserve the walker's newest-first order for the normal path so the
    // existing first-wins dedup behavior stays stable. Contributions retained
    // from a temporarily unreadable root follow in deterministic path order.
    let mut emitted = HashSet::new();
    let mut events = Vec::new();
    for path in &files {
        if emitted.insert(path.clone()) {
            if let Some(file_events) = state.by_file.get(path) {
                events.extend(file_events.iter().cloned());
            }
        }
    }
    for (path, file_events) in &state.by_file {
        if emitted.insert(path.clone()) {
            events.extend(file_events.iter().cloned());
        }
    }
    dedup_events(&mut events);

    ScanResult::Ok {
        events,
        files_scanned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn assistant_line(msg_id: &str, req_id: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z","requestId":"{req_id}","message":{{"id":"{msg_id}","model":"m","usage":{{"input_tokens":1,"output_tokens":{output}}}}}}}"#
        ) + "\n"
    }

    fn scanned_events(root: &std::path::Path, state: &mut ScanState) -> Vec<UsageEvent> {
        match scan_incremental(&[root.to_path_buf()], state) {
            ScanResult::Ok { events, .. } => events,
            ScanResult::Fatal { error } => panic!("scan failed: {error}"),
        }
    }

    fn advance_mtime(path: &std::path::Path) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn truncation_replaces_file_contribution_and_removes_vanished_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            assistant_line("msg_a", "req_1", 100) + &assistant_line("msg_b", "req_2", 200),
        )
        .unwrap();
        let mut state = ScanState::new();
        assert_eq!(scanned_events(dir.path(), &mut state).len(), 2);

        std::fs::write(&path, assistant_line("msg_c", "req_3", 300)).unwrap();
        let events = scanned_events(dir.path(), &mut state);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message_id.as_deref(), Some("msg_c"));
    }

    #[test]
    fn same_size_rewrite_replaces_changed_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let original = assistant_line("msg_a", "req_1", 100);
        let replacement = assistant_line("msg_b", "req_2", 200);
        assert_eq!(original.len(), replacement.len());
        std::fs::write(&path, original).unwrap();
        let mut state = ScanState::new();
        assert_eq!(scanned_events(dir.path(), &mut state)[0].output_tokens, 100);

        std::fs::write(&path, replacement).unwrap();
        advance_mtime(&path);
        let events = scanned_events(dir.path(), &mut state);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message_id.as_deref(), Some("msg_b"));
        assert_eq!(events[0].output_tokens, 200);
    }

    #[test]
    fn duplicate_ids_across_files_remain_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.jsonl"),
            assistant_line("msg_shared", "req_shared", 100),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.jsonl"),
            assistant_line("msg_shared", "req_shared", 100),
        )
        .unwrap();

        let events = scanned_events(dir.path(), &mut ScanState::new());
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn deleted_file_removes_its_owned_contribution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, assistant_line("msg_a", "req_1", 100)).unwrap();
        let mut state = ScanState::new();
        assert_eq!(scanned_events(dir.path(), &mut state).len(), 1);

        std::fs::remove_file(path).unwrap();
        assert!(scanned_events(dir.path(), &mut state).is_empty());
    }
}
