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
mod validate;

pub use errors::WatcherError;
pub use validate::{KeyProbe, validate_openai_key};

use settings::Settings;
use state_coordinator::StateCoordinatorHandle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub struct Watcher;

impl Watcher {
    /// Spawn all watcher tasks. Returns one `JoinHandle` per task.
    ///
    /// Default-enabled tasks (always spawned):
    /// 1. `jsonl` - notify-watches `~/.claude/projects/**/*.jsonl`; 300ms debounce,
    ///    plus a 60s incremental fallback. Reads via a per-file byte cursor
    ///    (AGENTS.md §3.1), so neither path does a full reparse after launch.
    /// 2. `statusline` - notify-watches `<data_dir>/statusline.snapshot.json`; 100ms debounce.
    /// 3. `safety` - 60s safety re-read of statusline + Codex (skips first tick).
    ///    Its Codex scan is gated on `settings.providers.codex_enabled`. JSONL is
    ///    not re-scanned here - the `jsonl` task's 60s fallback covers that.
    ///
    /// Conditionally-spawned tasks (each gated on a `ProviderSettings` toggle so
    /// a `false` value short-circuits before we spawn - no log spam, no API
    /// calls; the cell stays `None` until re-enabled. The Tauri host re-spawns
    /// the watcher on a settings change, so these apply live):
    /// 4. `openai_poll` - only when `providers.openai_enabled` is `true` OR a
    ///    non-empty `BALANZE_OPENAI_KEY` env override is set (that documented
    ///    power-user path bypasses the keychain and must keep working).
    /// 5. `oauth_poll` - only when `providers.anthropic_enabled` is `true` (the
    ///    default); disables Anthropic OAuth polling without removing the
    ///    credentials file.
    ///
    /// The returned `Vec` therefore has length 3 to 5 depending on settings.
    /// The caller (`balanze-cli --watch`, or the Tauri host) runs whatever
    /// handles come back under `tokio::select!`. A panic surfaces as
    /// `JoinError::is_panic() == true`; the supervisor's job is to log and
    /// (optionally) restart.
    ///
    /// Each handle is paired with a static label (`"jsonl"`, `"statusline"`,
    /// `"openai_poll"`, `"safety"`, `"oauth_poll"`) so the supervisor's
    /// logs don't drift out of sync with the spawn order if a future
    /// refactor reshuffles which task is spawned first. The label is the
    /// canonical name a maintainer would use to grep for the task - keep
    /// the strings in lockstep with the module names under
    /// `crates/watcher/src/tasks/`.
    pub fn spawn(
        handle: StateCoordinatorHandle,
        settings: &Settings,
    ) -> Vec<(&'static str, JoinHandle<Result<(), WatcherError>>)> {
        let mut tasks: Vec<(&'static str, JoinHandle<Result<(), WatcherError>>)> = vec![
            ("jsonl", tasks::jsonl::spawn(handle.clone())),
            ("statusline", tasks::statusline::spawn(handle.clone())),
            (
                "safety",
                tasks::safety::spawn(handle.clone(), settings.providers.codex_enabled),
            ),
        ];
        // OpenAI: gated on the toggle, OR on a present `BALANZE_OPENAI_KEY` env
        // override (the documented power-user path must keep working even with
        // the toggle off, since it bypasses the keychain entirely).
        if settings.providers.openai_enabled || openai_env_key_present() {
            tasks.push((
                "openai_poll",
                tasks::openai_poll::spawn(handle.clone(), settings.oauth_poll_interval_secs),
            ));
        }
        if settings.providers.anthropic_enabled {
            tasks.push((
                "oauth_poll",
                tasks::oauth_poll::spawn(handle.clone(), settings.oauth_poll_interval_secs),
            ));
        }
        tasks
    }
}

/// True if a non-empty `BALANZE_OPENAI_KEY` env override is set.
fn openai_env_key_present() -> bool {
    std::env::var("BALANZE_OPENAI_KEY").is_ok_and(|v| !v.trim().is_empty())
}

/// Spawn one watchdog task per `(label, handle)` returned by [`Watcher::spawn`]:
/// signal the *unexpected* completion (an `Err` return or a panic) of a watcher
/// task through the returned channel, so a supervisor's `tokio::select!` learns
/// of a failure without pulling in `JoinSet` / `select_all` (neither is a
/// workspace dep). A clean `Ok(())` exit (a provider that is simply not
/// configured - e.g. `openai_poll` with no key) and a deliberate cancellation
/// (a reload-abort) are NOT signalled: treating either as fatal would break
/// `watch` for the common single-provider user, or fire spuriously on shutdown.
///
/// Consumes the handles. A caller that also needs to abort the tasks later (the
/// Tauri host's live-reload path) should collect their `abort_handle()`s first.
pub fn watch_for_task_death(
    handles: Vec<(&'static str, JoinHandle<Result<(), WatcherError>>)>,
) -> mpsc::UnboundedReceiver<&'static str> {
    let (tx, rx) = mpsc::unbounded_channel();
    for (label, h) in handles {
        let tx = tx.clone();
        tokio::spawn(async move {
            match h.await {
                Ok(Ok(())) => tracing::debug!("watcher/{label}: exited Ok(())"),
                Ok(Err(e)) => {
                    tracing::error!("watcher/{label}: returned error: {e}");
                    let _ = tx.send(label);
                }
                Err(je) if je.is_cancelled() => {}
                Err(je) => {
                    tracing::error!("watcher/{label}: panicked/aborted: {je}");
                    let _ = tx.send(label);
                }
            }
        });
    }
    rx
}

/// Map a watcher task label (as returned by [`Watcher::spawn`]) to the
/// `state_coordinator::Source` whose data that task feeds. The host supervisor
/// uses this to surface a `degraded_state` for the right cell when a task dies
/// unexpectedly. Keep in lockstep with the labels in [`Watcher::spawn`].
///
/// `safety` maps to `CodexQuota`: Codex has no notify task of its own, so the
/// safety poll is its only feeder. The safety poll's other job (re-reading the
/// statusline snapshot) is just a backstop for the `statusline` notify task,
/// which keeps running, so statusline is not the cell left dark by a safety death.
pub fn source_for_label(label: &str) -> Option<state_coordinator::Source> {
    use state_coordinator::Source;
    match label {
        "jsonl" => Some(Source::ClaudeJsonl),
        "statusline" => Some(Source::ClaudeStatusline),
        "openai_poll" => Some(Source::OpenAiCosts),
        "oauth_poll" => Some(Source::ClaudeOAuth),
        "safety" => Some(Source::CodexQuota),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::source_for_label;
    use state_coordinator::Source;

    #[test]
    fn source_for_label_maps_every_spawn_label() {
        // Mirrors the labels in `Watcher::spawn`; a drift here means a dead task
        // would surface no (or the wrong) degraded cell.
        assert_eq!(source_for_label("jsonl"), Some(Source::ClaudeJsonl));
        assert_eq!(
            source_for_label("statusline"),
            Some(Source::ClaudeStatusline)
        );
        assert_eq!(source_for_label("openai_poll"), Some(Source::OpenAiCosts));
        assert_eq!(source_for_label("oauth_poll"), Some(Source::ClaudeOAuth));
        assert_eq!(source_for_label("safety"), Some(Source::CodexQuota));
        assert_eq!(source_for_label("nonexistent"), None);
    }
}
