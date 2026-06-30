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
use state_coordinator::{Sink, spawn as spawn_coord};
use tokio::sync::mpsc;
use watcher::Watcher;

use crate::sinks::{JsonlSink, StdoutSink};
use crate::tui::{ChannelSink, TuiExit, run_tui};

/// Entry-point called by `cmd_status` (and the `--watch` top-level alias)
/// when `--watch` is present.
///
/// * `json` - if `true`, uses [`JsonlSink`]; otherwise uses [`StdoutSink`].
pub(crate) fn run_watch_mode(json: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Enter the ratatui TUI only on an interactive stdout and when not emitting
    // JSON. Otherwise keep today's StdoutSink (separator-append) / JsonlSink
    // (one JSON doc per line) paths unchanged.
    use std::io::IsTerminal;
    let tui = !json && std::io::stdout().is_terminal();

    if tui {
        rt.block_on(run_tui_mode())
    } else if json {
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

    let (handle, coord_join) = match state_coordinator::snapshot_file_path() {
        Some(path) => spawn_coord(state_coordinator::SnapshotFileSink::new(sink, path)),
        None => spawn_coord(sink),
    };
    let watcher_handles = Watcher::spawn(handle.clone(), &settings);

    // Per-task watchdog: each watcher handle gets a wrapper that signals
    // *unexpected* completion (Err return OR panic) through a single mpsc,
    // so the top-level `select!` learns about any task failure without
    // needing `futures::select_all` or `JoinSet` (neither is a workspace
    // dep today). Clean `Ok(Ok(()))` exits - e.g. `openai_poll` when no
    // key is configured, `jsonl` when no Claude projects dir exists,
    // `oauth_poll` when credentials are absent at startup - are NOT
    // signalled. Each of those tasks is designed to exit clean when its
    // upstream isn't present; treating that as fatal would break
    // `--watch` for any user missing one of the providers (which is the
    // common case for the modal user).
    //
    // The labels come from `Watcher::spawn`'s return tuple rather than a
    // hard-coded array here, so they can't drift out of sync with the
    // spawn order if the watcher crate's task list changes.
    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<&'static str>();
    for (label, h) in watcher_handles {
        let tx = exit_tx.clone();
        tokio::spawn(async move {
            match h.await {
                Ok(Ok(())) => {
                    // Clean exit (e.g. no OpenAI key configured). Logged
                    // at debug; do NOT signal the supervisor.
                    tracing::debug!("watcher/{label}: exited Ok(())");
                }
                Ok(Err(e)) => {
                    tracing::error!("watcher/{label}: returned error: {e}");
                    let _ = tx.send(label);
                }
                Err(join_err) => {
                    tracing::error!("watcher/{label}: panicked or aborted: {join_err}");
                    let _ = tx.send(label);
                }
            }
        });
    }
    drop(exit_tx); // only the wrapper tasks hold senders now

    tokio::select! {
        res = tokio::signal::ctrl_c() => {
            // `ctrl_c()` returns io::Result<()> - installing the OS signal
            // handler can theoretically fail (process signal mask trouble
            // on exotic platforms). Surface that as an explicit error
            // rather than silently treating it like a real Ctrl-C.
            match res {
                Ok(()) => eprintln!("\nshutting down..."),
                Err(e) => {
                    tracing::error!("ctrl_c handler install failed: {e}");
                    return Err(anyhow::anyhow!(
                        "failed to install SIGINT handler: {e}"
                    ));
                }
            }
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

/// TUI variant of the watch supervisor. Spawns the coordinator with a
/// `ChannelSink` (republishes snapshots into a watch channel) + the watcher,
/// then runs the ratatui loop. `run_tui` owns the `TerminalGuard`; whichever
/// `select!` arm wins, dropping the loser futures on return drops that guard and
/// restores the terminal, so any fatal hint we print AFTER the block lands on
/// the normal screen, not a garbled alt screen. The runtime drop aborts
/// surviving tasks.
async fn run_tui_mode() -> Result<()> {
    let settings = settings::load().unwrap_or_else(|e| {
        tracing::warn!("settings load failed ({e}); using defaults");
        settings::Settings::default()
    });

    let (sink, rx) = ChannelSink::new();
    let (handle, mut coord_join) = match state_coordinator::snapshot_file_path() {
        Some(path) => spawn_coord(state_coordinator::SnapshotFileSink::new(sink, path)),
        None => spawn_coord(sink),
    };
    let watcher_handles = Watcher::spawn(handle.clone(), &settings);

    // Per-task watchdog mirroring `run_with_sink`: surface an unexpected watcher
    // Err-return or panic so a dead provider task is not silently invisible
    // behind the TUI (AGENTS.md §3.2 - long-running tasks must be supervised).
    // Clean Ok(()) exits (a provider that is simply not configured) are NOT
    // signalled - treating those as fatal would break `watch` for the common
    // single-provider user.
    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<&'static str>();
    for (label, h) in watcher_handles {
        let tx = exit_tx.clone();
        tokio::spawn(async move {
            match h.await {
                Ok(Ok(())) => tracing::debug!("watcher/{label}: exited Ok(())"),
                Ok(Err(e)) => {
                    tracing::error!("watcher/{label}: returned error: {e}");
                    let _ = tx.send(label);
                }
                Err(join_err) => {
                    tracing::error!("watcher/{label}: panicked or aborted: {join_err}");
                    let _ = tx.send(label);
                }
            }
        });
    }
    drop(exit_tx); // only the wrapper tasks hold senders now

    // Race the TUI against ctrl-c, the coordinator dying, and a watcher dying.
    // `&mut coord_join` lets the TUI arm win without consuming the join handle.
    let mut fatal: Option<String> = None;
    tokio::select! {
        res = run_tui(rx, handle.clone()) => {
            // A coordinator-gone exit (the run_tui arm winning the race) is
            // surfaced as fatal too, so a coordinator death is never mistaken
            // for a clean user quit.
            if let TuiExit::CoordinatorGone = res? {
                tracing::error!("snapshot channel closed: state coordinator gone");
                fatal = Some(
                    "state_coordinator task exited unexpectedly. \
                     See `BALANZE_LOG=debug` output for detail. Restart `watch` to recover."
                        .to_string(),
                );
            }
        }
        res = tokio::signal::ctrl_c() => {
            if let Err(e) = res {
                tracing::error!("ctrl_c handler install failed: {e}");
                return Err(anyhow::anyhow!("failed to install SIGINT handler: {e}"));
            }
        }
        res = &mut coord_join => {
            tracing::error!("coordinator task exited unexpectedly: {res:?}");
            fatal = Some(
                "state_coordinator task exited unexpectedly. \
                 See `BALANZE_LOG=debug` output for detail. Restart `watch` to recover."
                    .to_string(),
            );
        }
        Some(label) = exit_rx.recv() => {
            tracing::error!("watcher task '{label}' exited unexpectedly");
            fatal = Some(format!(
                "watcher task '{label}' exited unexpectedly. The data source it covers \
                 is no longer live. See `BALANZE_LOG=debug` output for detail. \
                 Restart `watch` to recover."
            ));
        }
    }
    // The select dropped run_tui's TerminalGuard on the way out, restoring the
    // terminal. Print any fatal hint NOW, on the normal screen (not the alt
    // screen, which the restore just tore down).
    if let Some(msg) = fatal {
        eprintln!("\nfatal: {msg}");
    }
    Ok(())
}
