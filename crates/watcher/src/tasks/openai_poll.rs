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
//! The fetch + error-mapping still mirrors `balanze_cli::live_fetch_openai`; a
//! full shared fetch helper is deliberately left un-merged because the two
//! paths diverge on backoff (`standard()` here vs the CLI's `fail_fast()`) and
//! on caching (the CLI's 300s self-compose disk cache).

use openai_client::{DEFAULT_API_BASE as OPENAI_API_BASE, costs_this_month};
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;
use crate::tasks::get_or_build_client;

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
        // slow `costs_this_month` under `BackoffPolicy::standard()` backoff
        // can't queue up multiple missed ticks and fire a burst on recovery.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut client: Option<reqwest::Client> = None;

        loop {
            ticker.tick().await;

            let client = match get_or_build_client(&mut client) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("watcher/openai_poll: reqwest client build failed: {e}");
                    let _ = coord
                        .send(StateMsg::Update(SourceUpdate {
                            source: Source::OpenAiCosts,
                            result: Err(format!("reqwest client build failed: {e}")),
                        }))
                        .await;
                    continue;
                }
            };

            let update = match costs_this_month(
                client,
                OPENAI_API_BASE,
                &key,
                &backoff::BackoffPolicy::standard(),
            )
            .await
            {
                Ok(costs) => {
                    tracing::info!(
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
                Err(openai_client::OpenAiError::AuthInvalid { .. }) => SourceUpdate {
                    source: Source::OpenAiCosts,
                    result: Err("OpenAI admin key rejected (HTTP 401). \
                         Run `balanze-cli set-openai-key` with a fresh `sk-admin-...` key."
                        .to_string()),
                },
                Err(openai_client::OpenAiError::InsufficientScope { .. }) => SourceUpdate {
                    source: Source::OpenAiCosts,
                    result: Err(
                        "OpenAI returned 403. organization/costs requires an admin API key \
                         (`sk-admin-...`), not a project or service-account key."
                            .to_string(),
                    ),
                },
                Err(e) => {
                    tracing::warn!("watcher/openai_poll: fetch error: {e}");
                    SourceUpdate {
                        source: Source::OpenAiCosts,
                        result: Err(format!("{e}")),
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
