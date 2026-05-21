//! Long-running watcher tasks that feed the `state_coordinator` actor.
//!
//! Per AGENTS.md §4 #4 boundary, this crate is the only one that imports
//! `notify`. Each task module exposes a `pub(crate) fn spawn(...)` returning
//! a `tokio::task::JoinHandle<Result<(), WatcherError>>`; `Watcher::spawn`
//! is the single entry-point that spawns them all and returns the collection.
//!
//! **5a:** JSONL notify task only.
//! **5b:** Adds statusline notify task, OAuth + OpenAI poll tasks, and the
//! 60s safety poll. `Watcher::spawn` now returns exactly 5 handles.

mod errors;
mod tasks;

pub use errors::WatcherError;

use settings::Settings;
use state_coordinator::StateCoordinatorHandle;
use tokio::task::JoinHandle;

pub struct Watcher;

impl Watcher {
    /// Spawn all watcher tasks. Returns one `JoinHandle` per task (5 total).
    ///
    /// Tasks:
    /// 1. `jsonl` — notify-watches `~/.claude/projects/**/*.jsonl`; 300ms debounce.
    /// 2. `statusline` — notify-watches `<data_dir>/statusline.snapshot.json`; 100ms debounce.
    /// 3. `oauth_poll` — polls Anthropic OAuth usage at `settings.oauth_poll_interval_secs` (min 60s).
    /// 4. `openai_poll` — polls OpenAI org costs at the same cadence.
    /// 5. `safety` — 60s safety re-scan of JSONL + statusline + Codex (skips first tick).
    ///
    /// The caller (Task 6's `balanze-cli --watch` supervisor) runs the returned
    /// handles under `tokio::select!`. A panic surfaces as
    /// `JoinError::is_panic() == true`; the supervisor's job is to log and
    /// (optionally) restart.
    pub fn spawn(
        handle: StateCoordinatorHandle,
        settings: &Settings,
    ) -> Vec<JoinHandle<Result<(), WatcherError>>> {
        vec![
            tasks::jsonl::spawn(handle.clone()),
            tasks::statusline::spawn(handle.clone()),
            tasks::oauth_poll::spawn(handle.clone(), settings.oauth_poll_interval_secs),
            tasks::openai_poll::spawn(handle.clone(), settings.oauth_poll_interval_secs),
            tasks::safety::spawn(handle.clone()),
        ]
    }
}
