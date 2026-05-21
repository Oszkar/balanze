//! OAuth poll task. Polls `GET /api/oauth/usage` at a configurable interval
//! (default 300s; minimum 60s enforced at call site). Each tick:
//! 1. Re-locates and re-loads credentials from disk (handles atomic rewrite by
//!    Claude Code between polls).
//! 2. Pre-flight refreshes the bearer if expired or near-expiry (within 5min).
//! 3. Calls `fetch_usage` with `BackoffPolicy::standard()`.
//! 4. On `AuthExpired`, refreshes once and retries (same pattern as the CLI).
//! 5. Emits `Update(ClaudeOAuth, ...)` to the state coordinator.
//!
//! MIRRORS balanze_cli::live_fetch_oauth and balanze_cli::refresh_and_persist
//! — see TODO(v0.2-followup): extract live_fetch crate so the CLI and watcher
//! share one implementation.

use anthropic_oauth::{
    fetch_usage, load_from as load_credentials_from, locate_credentials, refresh_access_token,
    write_back, CredentialsClaudeAiOauth, OAuthError, WriteBack, CLAUDE_CODE_CLIENT_ID,
    CLAUDE_CODE_TOKEN_URL, DEFAULT_API_BASE as ANTHROPIC_API_BASE,
};
use chrono::{Duration, Utc};
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

/// Pre-flight refresh margin: refresh the bearer if it expires within this window.
/// Mirrors `REFRESH_MARGIN` in `balanze_cli`.
const REFRESH_MARGIN: Duration = Duration::seconds(300);

/// Spawn the OAuth poll task and return its `JoinHandle`.
///
/// The task runs until the coordinator handle drops (coordinator shuts down).
/// Credential errors on a given tick emit an `Update(ClaudeOAuth, Err(...))`
/// and the task continues — transient failures (network hiccup, 429) are
/// handled by a `BackoffPolicy::standard()` that this task constructs and
/// passes into each `fetch_usage` call (the policy is per-call, not shared
/// across ticks — each tick gets a fresh retry budget).
///
/// `interval_secs` is clamped to a minimum of 60 inside this function so a
/// corrupt or hostile `settings.json` can't drive below the API-politeness
/// floor (AGENTS.md §3.1).
pub(crate) fn spawn(
    coord: StateCoordinatorHandle,
    interval_secs: u32,
) -> JoinHandle<Result<(), WatcherError>> {
    // Enforce the 60s API-politeness floor (AGENTS.md §3.1).
    let interval = std::time::Duration::from_secs(interval_secs.max(60) as u64);

    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .user_agent("balanze-watcher/0.1.0")
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("watcher/oauth_poll: reqwest client build failed: {e}");
                let _ = coord
                    .send(StateMsg::Update(SourceUpdate {
                        source: Source::ClaudeOAuth,
                        result: Err(format!("reqwest client build failed: {e}")),
                    }))
                    .await;
                return Ok(());
            }
        };

        // First tick fires immediately (interval fires on first `.tick()` call).
        // `Delay` (not the default `Burst`) so a slow tick — `fetch_usage`
        // backing off for up to 10 minutes under `BackoffPolicy::standard()` —
        // can't queue up multiple missed 5-min ticks and fire them
        // back-to-back when the network recovers. That would violate the
        // §3.1 API-politeness floor in exactly the worst conditions.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            let result = poll_once(&client).await;
            let update = match result {
                Ok(snapshot) => {
                    tracing::info!(
                        "watcher/oauth_poll: fetched {} cadence bars",
                        snapshot.cadences.len()
                    );
                    SourceUpdate {
                        source: Source::ClaudeOAuth,
                        result: Ok(SourcePartial::ClaudeOAuth(snapshot)),
                    }
                }
                Err(e) => {
                    tracing::warn!("watcher/oauth_poll: fetch error: {e}");
                    SourceUpdate {
                        source: Source::ClaudeOAuth,
                        result: Err(format!("{e}")),
                    }
                }
            };
            let _ = coord.send(StateMsg::Update(update)).await;
        }
    })
}

/// One poll tick: load credentials, pre-flight refresh if needed, fetch usage.
/// On `AuthExpired`, refresh + retry once (mirrors `live_fetch_oauth` in balanze_cli).
async fn poll_once(
    client: &reqwest::Client,
) -> anyhow::Result<anthropic_oauth::ClaudeOAuthSnapshot> {
    // Re-locate and re-load credentials on every tick: Claude Code may have
    // atomically rewritten `~/.claude/.credentials.json` between polls
    // (e.g. its own token refresh). Re-loading ensures we always have the
    // freshest on-disk token rather than a potentially stale in-memory copy.
    let path = locate_credentials()?;
    let creds = load_credentials_from(&path)?;
    let mut oauth = creds.claude_ai_oauth;

    // Pre-flight: refresh if the access token is expired or near-expiry.
    if token_needs_refresh(oauth.expires_at, Utc::now(), REFRESH_MARGIN) {
        tracing::info!("watcher/oauth_poll: token expired/near-expiry — refreshing pre-flight");
        oauth = refresh_and_persist(client, &path, oauth).await?;
    }

    let policy = backoff::BackoffPolicy::standard();

    match fetch_usage(
        client,
        ANTHROPIC_API_BASE,
        &oauth.access_token,
        oauth.subscription_type.clone(),
        oauth.rate_limit_tier.clone(),
        &policy,
    )
    .await
    {
        Ok(s) => Ok(s),
        Err(OAuthError::AuthExpired) => {
            // Pre-flight refresh already happened but we still got 401.
            // One more refresh + retry (bounded — the retry uses `?` so a
            // second AuthExpired propagates rather than looping).
            tracing::warn!("watcher/oauth_poll: 401 despite pre-flight — one refresh+retry");
            let oauth = refresh_and_persist(client, &path, oauth).await?;
            let s = fetch_usage(
                client,
                ANTHROPIC_API_BASE,
                &oauth.access_token,
                oauth.subscription_type,
                oauth.rate_limit_tier,
                &policy,
            )
            .await?;
            tracing::info!(
                "watcher/oauth_poll: fetched {} cadence bars after refresh",
                s.cadences.len()
            );
            Ok(s)
        }
        Err(e) => Err(e.into()),
    }
}

/// True if `expires_at_ms` is in the past or within `margin` of now.
/// Saturating sub prevents underflow on pathological/hostile timestamps.
fn token_needs_refresh(expires_at_ms: i64, now: chrono::DateTime<Utc>, margin: Duration) -> bool {
    now.timestamp_millis() >= expires_at_ms.saturating_sub(margin.num_milliseconds())
}

/// Refresh the bearer token and atomically persist it back to disk.
/// A write-back failure is non-fatal as long as we hold a usable in-memory token.
/// MIRRORS balanze_cli::refresh_and_persist.
async fn refresh_and_persist(
    client: &reqwest::Client,
    path: &std::path::Path,
    oauth: CredentialsClaudeAiOauth,
) -> anyhow::Result<CredentialsClaudeAiOauth> {
    let rt = oauth
        .refresh_token
        .as_deref()
        .ok_or(OAuthError::RefreshTokenMissing)?;
    // Watcher uses BackoffPolicy::standard() (30s start, 10min cap) —
    // unlike the one-shot CLI which uses fail_fast().
    let refreshed = refresh_access_token(
        client,
        CLAUDE_CODE_TOKEN_URL,
        CLAUDE_CODE_CLIENT_ID,
        rt,
        Utc::now().timestamp_millis(),
        &backoff::BackoffPolicy::standard(),
    )
    .await?;
    match write_back(path, &refreshed) {
        Ok(WriteBack::Written) => {
            tracing::info!("watcher/oauth_poll: refreshed bearer, wrote back")
        }
        Ok(WriteBack::SkippedDiskNewer) => {
            tracing::info!(
                "watcher/oauth_poll: refreshed bearer; on-disk copy already newer, kept disk"
            )
        }
        Err(e) => {
            tracing::warn!("watcher/oauth_poll: refresh ok but write-back failed (non-fatal): {e}")
        }
    }
    let mut next = oauth;
    next.access_token = refreshed.access_token;
    next.refresh_token = Some(refreshed.refresh_token);
    next.expires_at = refreshed.expires_at_ms;
    Ok(next)
}
