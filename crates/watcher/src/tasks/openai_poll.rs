//! OpenAI cost poll task. Polls `GET /v1/organization/costs` at a configurable
//! interval (shared with `oauth_poll`; default 300s; minimum 60s enforced here).
//! Each tick: resolve the OpenAI admin key → fetch month-to-date costs →
//! emit `Update(OpenAiCosts, ...)`.
//!
//! Key resolution order (same as the CLI):
//!   1. `BALANZE_OPENAI_KEY` env var (workaround for Windows keychain bug).
//!   2. OS keychain entry `openai_api_key`.
//!   3. Neither configured → log at `info!` and exit `Ok(())` immediately.
//!
//! MIRRORS balanze_cli::live_fetch_openai — see
//! TODO(v0.2-followup): extract live_fetch crate so CLI and watcher share
//! one implementation.

use openai_client::{costs_this_month, DEFAULT_API_BASE as OPENAI_API_BASE};
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

/// Spawn the OpenAI cost poll task and return its `JoinHandle`.
///
/// If no key is configured the task exits `Ok(())` immediately — the OpenAI
/// cell stays blank (no `Update` emitted). Subsequent ticks are never reached
/// because the task has exited; the OpenAI cell only populates if the user adds
/// a key and restarts the watcher (or the Tauri app).
///
/// `interval_secs` is clamped to a minimum of 60 (API-politeness floor,
/// AGENTS.md §3.1).
pub(crate) fn spawn(
    coord: StateCoordinatorHandle,
    interval_secs: u32,
) -> JoinHandle<Result<(), WatcherError>> {
    let interval = std::time::Duration::from_secs(interval_secs.max(60) as u64);

    tokio::spawn(async move {
        // Resolve the key once at task startup. The key rarely changes during
        // a watcher session; if the user adds/rotates it they restart the app.
        let key = match resolve_key().await {
            Some(k) => k,
            None => {
                tracing::info!(
                    "watcher/openai_poll: no OpenAI admin key configured; task exits clean"
                );
                return Ok(());
            }
        };

        let client = match reqwest::Client::builder()
            .user_agent("balanze-watcher/0.1.0")
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("watcher/openai_poll: reqwest client build failed: {e}");
                let _ = coord
                    .send(StateMsg::Update(SourceUpdate {
                        source: Source::OpenAiCosts,
                        result: Err(format!("reqwest client build failed: {e}")),
                    }))
                    .await;
                return Ok(());
            }
        };

        // First tick fires immediately.
        let mut ticker = tokio::time::interval(interval);

        loop {
            ticker.tick().await;

            let update = match costs_this_month(
                &client,
                OPENAI_API_BASE,
                &key,
                &backoff::BackoffPolicy::standard(),
            )
            .await
            {
                Ok(costs) => {
                    tracing::info!(
                        "watcher/openai_poll: fetched costs total_usd={} buckets={} truncated={}",
                        costs.total_usd,
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
                         Run `balanze-cli set-openai-key` with a fresh `sk-admin-…` key."
                        .to_string()),
                },
                Err(openai_client::OpenAiError::InsufficientScope { .. }) => SourceUpdate {
                    source: Source::OpenAiCosts,
                    result: Err(
                        "OpenAI returned 403. organization/costs requires an admin API key \
                         (`sk-admin-…`), not a project or service-account key."
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

/// Resolve the OpenAI admin key from env var or keychain.
/// Returns `None` if not configured (no error — just not set up).
/// Errors from the keychain other than `NotFound` are logged but treated as
/// "not configured" to avoid a boot failure blocking the rest of the watcher.
async fn resolve_key() -> Option<String> {
    if let Ok(env_key) = std::env::var("BALANZE_OPENAI_KEY") {
        let trimmed = env_key.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
        // Empty env var = "not configured".
        return None;
    }
    match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(k) => Some(k),
        Err(keychain::KeychainError::NotFound(_)) => None,
        Err(e) => {
            tracing::warn!("watcher/openai_poll: keychain error (treating as not configured): {e}");
            None
        }
    }
}
