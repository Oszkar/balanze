//! `--watch` mode orchestration.
//!
//! Builds a multi-thread tokio runtime, spawns the coordinator + watcher,
//! then blocks until Ctrl-C. Exit explicitly joins watcher tasks, drops the
//! final coordinator handle, and awaits coordinator shutdown so the coalescing
//! snapshot writer can flush or report its final pending state.
//!
//! # Type-inference note
//!
//! `state_coordinator::spawn` is generic over `S: Sink`. The two call sites
//! (`StdoutSink` vs `JsonlSink`) return different concrete types, so we factor
//! the common post-spawn logic into `run_with_sink` instead of putting a
//! `match` inside `block_on` and fighting the borrow checker over mismatched
//! branches.

use anyhow::Result;
use state_coordinator::Sink;
use watcher::Watcher;

use crate::sinks::{JsonlSink, StdoutSink};
use crate::tui::{ChannelSink, TuiExit, run_tui};

/// Entry-point called by `cmd_status` (and the `--watch` top-level alias)
/// when `--watch` is present.
///
/// * `json` - if `true`, uses [`JsonlSink`]; otherwise uses [`StdoutSink`].
/// * `verbose` - when `json=true`, controls identifier redaction in JSONL.
pub(crate) fn run_watch_mode(json: bool, verbose: bool) -> Result<()> {
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
        rt.block_on(run_with_sink(JsonlSink::new(verbose)))
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
    let settings = settings::load_or_default();

    let (handle, mut coord_join) = state_coordinator::spawn_with_optional_file(sink);
    handle.transition_settings(settings.clone(), 1).await?;
    let watcher_handles = Watcher::spawn(handle.clone(), &settings, 1);

    // Per-task watchdog: `watch_for_task_death` signals *unexpected* completion
    // (an Err return or a panic) of any watcher task through this channel, so the
    // `select!` below learns of a failure without a `JoinSet`/`select_all` dep.
    // Clean `Ok(())` exits - a provider simply not configured (no OpenAI key, no
    // Claude dir, absent credentials) - are NOT signalled; treating them as fatal
    // would break `--watch` for any single-provider user. The labels come from
    // `Watcher::spawn`, so they can't drift out of sync with the spawn order.
    let mut watched = watcher::watch_for_task_death(watcher_handles);

    let mut coordinator_exited = false;
    let mut command_error = None;
    let mut fatal = None;
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
                    command_error = Some(anyhow::anyhow!("failed to install SIGINT handler: {e}"));
                }
            }
        }
        res = &mut coord_join => {
            coordinator_exited = true;
            tracing::error!("coordinator task exited unexpectedly: {res:?}");
            fatal = Some(
                "state_coordinator task exited unexpectedly. \
                 See `BALANZE_LOG=debug` output for detail. Restart `--watch` to recover."
                    .to_string(),
            );
        }
        Some(label) = watched.recv_death() => {
            tracing::error!("watcher task '{label}' exited unexpectedly");
            fatal = Some(format!(
                "watcher task '{label}' exited unexpectedly. \
                 The data source it covers is no longer live. \
                 See `BALANZE_LOG=debug` output for detail. Restart `--watch` to recover."
            ));
        }
    }
    watched.shutdown().await;
    drop(handle);
    if !coordinator_exited {
        coord_join
            .await
            .map_err(|error| anyhow::anyhow!("coordinator shutdown failed: {error}"))?;
    }
    finish_watch(command_error, fatal)
}

/// Fold the supervisor outcome into the command result. Fatal task death must
/// cross the CLI boundary as an error so `main` returns `ExitClass::Other` (1),
/// while Ctrl-C and a user TUI quit remain successful exits.
fn finish_watch(command_error: Option<anyhow::Error>, fatal: Option<String>) -> Result<()> {
    match (command_error, fatal) {
        (Some(error), _) => Err(error),
        (None, Some(message)) => Err(anyhow::anyhow!(message)),
        (None, None) => Ok(()),
    }
}

/// TUI variant of the watch supervisor. Spawns the coordinator with a
/// `ChannelSink` (republishes snapshots into a watch channel) + the watcher,
/// then runs the ratatui loop. `run_tui` owns the `TerminalGuard`; whichever
/// `select!` arm wins, dropping the loser futures on return drops that guard and
/// restores the terminal, so any fatal hint we print AFTER the block lands on
/// the normal screen, not a garbled alt screen. Both exit paths then join the
/// live state tasks before returning.
async fn run_tui_mode() -> Result<()> {
    let settings = settings::load_or_default();

    let (sink, rx) = ChannelSink::new();
    let (handle, mut coord_join) = state_coordinator::spawn_with_optional_file(sink);
    handle.transition_settings(settings.clone(), 1).await?;
    let watcher_handles = Watcher::spawn(handle.clone(), &settings, 1);

    // Per-task watchdog mirroring `run_with_sink` (AGENTS.md §3.2): surface an
    // unexpected watcher Err-return or panic so a dead provider task is not
    // silently invisible behind the TUI. Clean `Ok(())` exits (a provider simply
    // not configured) are NOT signalled.
    let mut watched = watcher::watch_for_task_death(watcher_handles);

    // Race the TUI against ctrl-c, the coordinator dying, and a watcher dying.
    // `&mut coord_join` lets the TUI arm win without consuming the join handle.
    let mut fatal: Option<String> = None;
    let mut command_error = None;
    let mut coordinator_exited = false;
    tokio::select! {
        res = run_tui(rx, handle.clone()) => {
            // A coordinator-gone exit (the run_tui arm winning the race) is
            // surfaced as fatal too, so a coordinator death is never mistaken
            // for a clean user quit.
            match res {
                Ok(TuiExit::CoordinatorGone) => {
                    tracing::error!("snapshot channel closed: state coordinator gone");
                    fatal = Some(
                        "state_coordinator task exited unexpectedly. \
                         See `BALANZE_LOG=debug` output for detail. Restart `watch` to recover."
                            .to_string(),
                    );
                }
                Ok(TuiExit::UserQuit) => {}
                Err(error) => command_error = Some(error),
            }
        }
        res = tokio::signal::ctrl_c() => {
            if let Err(e) = res {
                tracing::error!("ctrl_c handler install failed: {e}");
                command_error = Some(anyhow::anyhow!("failed to install SIGINT handler: {e}"));
            }
        }
        res = &mut coord_join => {
            coordinator_exited = true;
            tracing::error!("coordinator task exited unexpectedly: {res:?}");
            fatal = Some(
                "state_coordinator task exited unexpectedly. \
                 See `BALANZE_LOG=debug` output for detail. Restart `watch` to recover."
                    .to_string(),
            );
        }
        Some(label) = watched.recv_death() => {
            tracing::error!("watcher task '{label}' exited unexpectedly");
            fatal = Some(format!(
                "watcher task '{label}' exited unexpectedly. The data source it covers \
                 is no longer live. See `BALANZE_LOG=debug` output for detail. \
                 Restart `watch` to recover."
            ));
        }
    }
    watched.shutdown().await;
    drop(handle);
    if !coordinator_exited {
        coord_join
            .await
            .map_err(|error| anyhow::anyhow!("coordinator shutdown failed: {error}"))?;
    }
    // The select dropped run_tui's TerminalGuard on the way out, restoring the
    // terminal. Returning the fatal error now lets `main` print it on the normal
    // screen and classify the process exit as non-zero.
    finish_watch(command_error, fatal)
}

#[cfg(test)]
mod tests {
    use super::finish_watch;

    #[test]
    fn clean_watch_shutdown_is_successful() {
        assert!(finish_watch(None, None).is_ok());
    }

    #[test]
    fn fatal_watcher_and_coordinator_shutdowns_are_errors() {
        for message in [
            "watcher task exited unexpectedly",
            "state_coordinator task exited unexpectedly",
        ] {
            let error = finish_watch(None, Some(message.to_string()))
                .expect_err("fatal supervisor outcome must produce exit code 1");
            assert_eq!(error.to_string(), message);
        }
    }
}
