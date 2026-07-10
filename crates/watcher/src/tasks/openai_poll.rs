//! OpenAI cost poll task. Polls `GET /v1/organization/costs` at a configurable
//! interval (shared with `oauth_poll`; default 300s; clamped to a 300s floor
//! here per AGENTS.md §3.1).
//! Each tick: resolve the OpenAI admin key → fetch month-to-date costs →
//! emit `Update(OpenAiCosts, ...)`.
//!
//! Key resolution is the shared `keychain::resolve_openai_key` (the
//! `BALANZE_OPENAI_KEY` env override, else the `openai_api_key` keychain entry);
//! if neither is configured the task logs at `info!` and exits `Ok(())`.
//!
//! The fetch shares `openai_client::costs_this_month_with` (client assembly +
//! this-month costs) with the CLI and self-compose paths, injecting this task's
//! own 30s timeout and `standard()` backoff; the 401/403 admin-key hint is the
//! shared `OpenAiError::admin_key_hint`, kept in lockstep with the CLI.

use openai_client::{DEFAULT_API_BASE as OPENAI_API_BASE, costs_this_month_with};
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

/// Spawn the OpenAI cost poll task and return its `JoinHandle`.
///
/// If no key is configured the task exits `Ok(())` immediately - the OpenAI
/// cell stays blank (no `Update` emitted). Subsequent ticks are never reached
/// because the task has exited; the OpenAI cell only populates if the user adds
/// a key and restarts the watcher (or the Tauri app).
///
/// `interval_secs` is clamped to a minimum of 300 (the 5-minute
/// API-politeness floor per AGENTS.md §3.1 - OpenAI billing data updates
/// infrequently and aggressive polling burns the user's rate quota for
/// no gain).
pub(crate) fn spawn(
    coord: StateCoordinatorHandle,
    interval_secs: u32,
) -> JoinHandle<Result<(), WatcherError>> {
    let interval = std::time::Duration::from_secs(interval_secs.max(300) as u64);

    tokio::spawn(async move {
        // Resolve the key once at task startup. The key rarely changes during
        // a watcher session; if the user adds/rotates it they restart the app.
        let key = match resolve_key() {
            Some(k) => k,
            None => {
                tracing::info!(
                    "watcher/openai_poll: no OpenAI admin key configured; task exits clean"
                );
                return Ok(());
            }
        };

        // First tick fires immediately. `Delay` (not default `Burst`) so a
        // slow `costs_this_month_with` under `BackoffPolicy::standard()` backoff
        // can't queue up multiple missed ticks and fire a burst on recovery.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            let update = match costs_this_month_with(
                OPENAI_API_BASE,
                &key,
                std::time::Duration::from_secs(30),
                &backoff::BackoffPolicy::standard(),
            )
            .await
            {
                Ok(costs) => {
                    // Per-tick success detail: debug, not info. INFO stays for
                    // lifecycle moments (no-key startup exit, errors) per §3.2 -
                    // this fires every poll and would otherwise scroll the
                    // `watch` TUI / stderr with unchanged `total=0` lines.
                    tracing::debug!(
                        "watcher/openai_poll: fetched costs total_micro_usd={} buckets={} truncated={}",
                        costs.total_micro_usd,
                        costs.by_line_item.len(),
                        costs.truncated
                    );
                    SourceUpdate {
                        source: Source::OpenAiCosts,
                        result: Ok(SourcePartial::OpenAiCosts(costs)),
                    }
                }
                Err(e) => {
                    // Shared 401/403 admin-key hint (in lockstep with the CLI);
                    // a client-build failure or any other error surfaces via
                    // Display with a WARN.
                    let result = match e.admin_key_hint() {
                        Some(hint) => Err(hint.to_string()),
                        None => {
                            tracing::warn!("watcher/openai_poll: fetch error: {e}");
                            Err(format!("{e}"))
                        }
                    };
                    SourceUpdate {
                        source: Source::OpenAiCosts,
                        result,
                    }
                }
            };
            let _ = coord.send(StateMsg::Update(update)).await;
        }
    })
}

/// Resolve the OpenAI admin key via the shared [`keychain::resolve_openai_key`]
/// (env override, else keychain). A real keychain failure is logged and treated
/// as "not configured" so a transient keychain error doesn't block the rest of
/// the watcher from booting.
fn resolve_key() -> Option<String> {
    keychain::resolve_openai_key().unwrap_or_else(|e| {
        tracing::warn!("watcher/openai_poll: keychain error (treating as not configured): {e}");
        None
    })
}
