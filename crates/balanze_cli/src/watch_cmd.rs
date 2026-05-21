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

/// Async body shared by both sink types.
async fn run_with_sink<S: Sink>(sink: S) -> Result<()> {
    let settings = settings::load().unwrap_or_else(|e| {
        tracing::warn!("settings load failed ({e}); using defaults");
        settings::Settings::default()
    });

    // Spawn the coordinator actor. The JoinHandle is kept alive for the
    // duration of the watch session but we don't join it explicitly — the
    // runtime drop on function return aborts it cleanly.
    let (handle, _coord_join) = spawn_coord(sink);

    // Spawn all watcher tasks (JSONL notify, statusline notify, OAuth/OpenAI
    // pollers, 60s safety poll). Returns 4 or 5 JoinHandles per Watcher::spawn
    // docs. We hold the Vec so the handles stay alive until Ctrl-C.
    let _tasks = Watcher::spawn(handle.clone(), &settings);

    // Block until the user presses Ctrl-C. tokio aborts all remaining spawned
    // tasks when the runtime is dropped after block_on returns.
    tokio::signal::ctrl_c().await?;
    eprintln!("\nshutting down\u{2026}");
    Ok(())
}
