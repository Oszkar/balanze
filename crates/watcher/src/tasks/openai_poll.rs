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
//! The fetch goes through the provider-owned 300-second gate shared by every
//! Costs caller. Each eligible tick performs one fail-fast HTTP request.

use state_coordinator::{
    Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg, WatcherGeneration,
};
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
    generation: WatcherGeneration,
) -> JoinHandle<Result<(), WatcherError>> {
    let interval = std::time::Duration::from_secs(interval_secs.max(300) as u64);

    tokio::spawn(async move {
        // Resolve the key once at task startup. The key rarely changes during
        // a watcher session; if the user adds/rotates it they restart the app.
        let key = match tokio::task::spawn_blocking(resolve_key).await {
            Ok(key) => key,
            Err(error) => {
                return Err(WatcherError::Io(std::io::Error::other(format!(
                    "OpenAI key resolution worker failed: {error}"
                ))));
            }
        };
        let key = match key {
            Some(k) => k,
            None => {
                tracing::info!(
                    "watcher/openai_poll: no OpenAI admin key configured; task exits clean"
                );
                return Ok(());
            }
        };

        // First tick fires immediately. `Delay` avoids queuing missed ticks.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            let result = openai_client::gated_costs_this_month(
                &openai_client::api_base_url(),
                &key,
                std::time::Duration::from_secs(30),
            )
            .await;
            let update = update_from_result(generation, result);
            let _ = coord.send(StateMsg::Update(update)).await;
        }
    })
}

fn update_from_result(
    generation: WatcherGeneration,
    result: Result<openai_client::OpenAiCosts, openai_client::CostsGateError>,
) -> SourceUpdate {
    match result {
        Ok(costs) => {
            tracing::debug!(
                "watcher/openai_poll: fetched costs total_micro_usd={} buckets={} truncated={}",
                costs.total_micro_usd,
                costs.by_line_item.len(),
                costs.truncated
            );
            SourceUpdate {
                generation,
                source: Source::OpenAiCosts,
                result: Ok(SourcePartial::OpenAiCosts(costs)),
            }
        }
        Err(error) => {
            let result = match error.admin_key_hint() {
                Some(hint) => Err(hint.to_string()),
                None => {
                    tracing::warn!("watcher/openai_poll: fetch error: {error}");
                    Err(error.to_string())
                }
            };
            SourceUpdate {
                generation,
                source: Source::OpenAiCosts,
                result,
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone as _, Utc};

    #[test]
    fn cached_success_maps_to_the_existing_coordinator_update() {
        let now = Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0).unwrap();
        let costs = openai_client::OpenAiCosts {
            start_time: Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
            end_time: now,
            total_micro_usd: 42,
            by_line_item: Vec::new(),
            truncated: false,
            fetched_at: now,
        };
        let update = update_from_result(7, Ok(costs));
        assert_eq!(update.generation, 7);
        assert!(matches!(
            update.result,
            Ok(SourcePartial::OpenAiCosts(value)) if value.total_micro_usd == 42
        ));
    }

    #[test]
    fn gate_deferral_maps_to_an_error_update_without_schema_changes() {
        let error = openai_client::CostsGateError::Deferred {
            reason: openai_client::GateDeferredReason::PriorMonth,
            retry_after_secs: 299,
            failure: None,
            cached: None,
        };
        let update = update_from_result(8, Err(error));
        assert_eq!(update.generation, 8);
        assert!(matches!(update.result, Err(message) if message.contains("299s")));
    }
}
