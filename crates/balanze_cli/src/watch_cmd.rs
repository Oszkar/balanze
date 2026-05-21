//! `--watch` mode orchestration.
//!
//! Builds a multi-thread tokio runtime, spawns the coordinator + watcher,
//! then blocks until Ctrl-C. The runtime drop aborts all spawned tasks, so
//! the watcher and coordinator shut down cleanly without explicit cancellation
//! tokens.
//!
//! # Type-inference note
//!
//! `state_coordinator::spawn` is generic over `S: Sink`. The two call sites
//! (`StdoutSink` vs `JsonlSink`) return different concrete types, so we factor
//! the common post-spawn logic into `run_with_sink` instead of putting a
//! `match` inside `block_on` and fighting the borrow checker over mismatched
//! branches.

use anyhow::Result;
use state_coordinator::{spawn as spawn_coord, Sink};
use tokio::sync::mpsc;
use watcher::Watcher;

use crate::sinks::{JsonlSink, StdoutSink};

/// Entry-point called by `cmd_status` (and the `--watch` top-level alias)
/// when `--watch` is present.
///
/// * `json` — if `true`, uses [`JsonlSink`]; otherwise uses [`StdoutSink`].
pub(crate) fn run_watch_mode(json: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    if json {
        rt.block_on(run_with_sink(JsonlSink))
    } else {
        rt.block_on(run_with_sink(StdoutSink::new()))
    }
}

/// Async body shared by both sink types. Acts as the supervisor for the
/// coordinator + watcher tasks per AGENTS.md §3.2 / §4 #4: a panic in any
/// of them surfaces here as a join completion, gets logged, and triggers
/// process exit. Without supervision the watch loop could go silently dead
/// (sink mid-render, no further output, no exit) while the user sees a
/// frozen TUI and assumes the watcher is just idle.
async fn run_with_sink<S: Sink>(sink: S) -> Result<()> {
    let settings = settings::load().unwrap_or_else(|e| {
        tracing::warn!("settings load failed ({e}); using defaults");
        settings::Settings::default()
    });

    let (handle, coord_join) = spawn_coord(sink);
    let watcher_handles = Watcher::spawn(handle.clone(), &settings);

    // Per-task watchdog: each watcher handle gets a wrapper that signals
    // completion (success OR panic OR Err) through a single mpsc, so the
    // top-level `select!` learns about any task exit without needing
    // `futures::select_all` or `JoinSet` (neither is a workspace dep
    // today). The wrapper tasks are short-lived stubs — they don't add
    // meaningful overhead vs. holding the bare Vec<JoinHandle>.
    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<&'static str>();
    let task_labels = ["jsonl", "statusline", "openai_poll", "safety", "oauth_poll"];
    for (i, h) in watcher_handles.into_iter().enumerate() {
        let label = task_labels.get(i).copied().unwrap_or("watcher-task");
        let tx = exit_tx.clone();
        tokio::spawn(async move {
            match h.await {
                Ok(Ok(())) => tracing::debug!("watcher/{label}: exited Ok(())"),
                Ok(Err(e)) => tracing::error!("watcher/{label}: returned error: {e}"),
                Err(join_err) => {
                    tracing::error!("watcher/{label}: panicked or aborted: {join_err}");
                }
            }
            let _ = tx.send(label);
        });
    }
    drop(exit_tx); // only the wrapper tasks hold senders now

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nshutting down...");
        }
        res = coord_join => {
            tracing::error!("coordinator task exited unexpectedly: {res:?}");
            eprintln!(
                "\nfatal: state_coordinator task exited unexpectedly. \
                 See `BALANZE_LOG=debug` output for detail. Restart `--watch` to recover."
            );
        }
        Some(label) = exit_rx.recv() => {
            tracing::error!("watcher task '{label}' exited unexpectedly");
            eprintln!(
                "\nfatal: watcher task '{label}' exited unexpectedly. \
                 The data source it covers is no longer live. \
                 See `BALANZE_LOG=debug` output for detail. Restart `--watch` to recover."
            );
        }
    }
    // Runtime drop on return aborts any tasks still running cleanly.
    Ok(())
}
