//! Long-running watcher tasks that feed the `state_coordinator` actor.
//!
//! Per AGENTS.md §4 #4 boundary, this crate is the only one that imports
//! `notify`. Each task module exposes a `pub(crate) fn spawn(...)` returning
//! a `tokio::task::JoinHandle<Result<(), WatcherError>>`; `Watcher::spawn`
//! is the single entry-point that spawns them all and returns the collection.
//!
//! **Scope of 5a:** JSONL notify task only. 5b adds the statusline notify
//! task, OAuth + OpenAI poll tasks, and the 60s safety poll.

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
    /// The caller (Task 6's `balanze-cli --watch` supervisor) runs the
    /// returned handles under `tokio::select!`. A panic surfaces here as
    /// `JoinError::is_panic() == true`; the supervisor's job is to log
    /// and (optionally) restart.
    ///
    /// **5a returns 1 handle** (the JSONL task). Task 5b grows the returned
    /// `Vec` to include the statusline notify task, OAuth + OpenAI poll tasks,
    /// and the 60s safety poll — the signature is final, only the length grows.
    pub fn spawn(
        handle: StateCoordinatorHandle,
        _settings: &Settings,
    ) -> Vec<JoinHandle<Result<(), WatcherError>>> {
        vec![tasks::jsonl::spawn(handle.clone())]
    }
}
