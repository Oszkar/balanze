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
    /// Spawn all watcher tasks. Returns one `JoinHandle` per task.
    ///
    /// Default-enabled tasks (always spawned):
    /// 1. `jsonl` — notify-watches `~/.claude/projects/**/*.jsonl`; 300ms debounce.
    /// 2. `statusline` — notify-watches `<data_dir>/statusline.snapshot.json`; 100ms debounce.
    /// 3. `openai_poll` — polls OpenAI org costs at `settings.oauth_poll_interval_secs` (min 300s); exits clean if no key configured.
    /// 4. `safety` — 60s safety re-scan of JSONL + statusline + Codex (skips first tick).
    ///
    /// Conditionally-spawned tasks:
    /// 5. `oauth_poll` — only when `settings.providers.anthropic_enabled` is `true` (the default).
    ///    The toggle is documented (`ProviderSettings::anthropic_enabled`) as
    ///    a way to disable Anthropic OAuth polling without removing the
    ///    credentials file. So a `false` value short-circuits before we spawn
    ///    the task at all — no log spam, no API calls, the OAuth Snapshot
    ///    cell stays `None` until the user re-enables and restarts.
    ///
    /// The returned `Vec` therefore has length 4 or 5 depending on settings.
    /// The caller (Task 6's `balanze-cli --watch` supervisor) runs whatever
    /// handles come back under `tokio::select!`. A panic surfaces as
    /// `JoinError::is_panic() == true`; the supervisor's job is to log and
    /// (optionally) restart.
    pub fn spawn(
        handle: StateCoordinatorHandle,
        settings: &Settings,
    ) -> Vec<JoinHandle<Result<(), WatcherError>>> {
        let mut tasks = vec![
            tasks::jsonl::spawn(handle.clone()),
            tasks::statusline::spawn(handle.clone()),
            tasks::openai_poll::spawn(handle.clone(), settings.oauth_poll_interval_secs),
            tasks::safety::spawn(handle.clone()),
        ];
        if settings.providers.anthropic_enabled {
            tasks.push(tasks::oauth_poll::spawn(
                handle.clone(),
                settings.oauth_poll_interval_secs,
            ));
        }
        tasks
    }
}
