//! OAuth poll task. Polls `GET /api/oauth/usage` at a configurable interval
//! (default 300s; clamped to a 300s floor at the call site per §3.1). Each tick:
//! 1. Re-locates and re-loads credentials from disk (handles atomic rewrite by
//!    Claude Code between polls).
//! 2. Pre-flight refreshes the bearer if expired or near-expiry (within 5min).
//! 3. Calls `fetch_usage` with `BackoffPolicy::standard()`.
//! 4. On `AuthExpired`, refreshes once and retries (same pattern as the CLI).
//! 5. Emits `Update(ClaudeOAuth, ...)` to the state coordinator.
//!
//! MIRRORS balanze_cli::live_fetch_oauth and balanze_cli::refresh_and_persist
//! — see TODO: extract a shared live-fetch helper so the CLI and watcher
//! share one implementation.

use anthropic_oauth::{
    CLAUDE_CODE_CLIENT_ID, CLAUDE_CODE_TOKEN_URL, CredentialsClaudeAiOauth,
    DEFAULT_API_BASE as ANTHROPIC_API_BASE, OAuthError, WriteBack, fetch_usage, load_from_source,
    locate_credentials, refresh_access_token, write_back,
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
/// `interval_secs` is clamped to a minimum of 300 inside this function so a
/// corrupt or hostile `settings.json` can't drive below the API-politeness
/// floor (AGENTS.md §3.1 — 5 minutes for provider usage/billing endpoints).
pub(crate) fn spawn(
    coord: StateCoordinatorHandle,
    interval_secs: u32,
) -> JoinHandle<Result<(), WatcherError>> {
    // Enforce the 5-minute (300s) API-politeness floor per AGENTS.md §3.1.
    // The setting default is also 300s, so this clamp only kicks in if a
    // user (or a corrupt settings.json) tries to set a smaller value.
    let interval = std::time::Duration::from_secs(interval_secs.max(300) as u64);

    tokio::spawn(async move {
        // Startup gate: if Claude Code isn't installed (no credentials file),
        // exit cleanly with an info log rather than emit an OAuth error
        // every 5 minutes for the lifetime of the process. The user can
        // install Claude Code + restart the watcher to pick it up - same
        // pattern as the JSONL task, which exits clean at startup if
        // `~/.claude/projects/` doesn't exist. Once we've seen credentials
        // here, transient `CredentialsMissing` errors during the loop
        // (e.g. user deletes the file mid-session) DO get reported as
        // Update errors - the watcher noticed and surfacing them is
        // helpful.
        //
        // Run on a blocking worker: locate+load is sync I/O (a file read, or a
        // `security` subprocess on macOS that can block on a Keychain access
        // prompt - this is the first credential touch and the likeliest to
        // prompt), and must not stall a tokio runtime thread (AGENTS.md §2.1).
        let startup_probe = tokio::task::spawn_blocking(|| {
            locate_credentials().and_then(|src| load_from_source(&src))
        })
        .await;
        if let Ok(Err(OAuthError::CredentialsMissing { .. })) = startup_probe {
            tracing::info!(
                "watcher/oauth_poll: no Claude credentials at startup; task exits clean. \
                 Install Claude Code and restart `--watch` to enable OAuth polling."
            );
            // Report a NEUTRAL "not configured" state (not an error) so the UI
            // shows "Claude Code not detected" instead of a perpetual loading
            // skeleton. A later restart with credentials clears it on first poll.
            let _ = coord
                .send(StateMsg::SourceUnavailable {
                    source: Source::ClaudeOAuth,
                    reason: "Claude Code not detected".to_string(),
                })
                .await;
            return Ok(());
        }

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

            // Build the client per tick. A build failure (e.g. TLS backend
            // init) previously `return Ok(())`'d — which the supervisor reads
            // as a clean exit, silently freezing the OAuth cell until restart.
            // Emitting the error + `continue` keeps the task alive: the cell
            // shows a persistent degraded state and the next tick retries.
            let client = match reqwest::Client::builder()
                .user_agent("balanze-watcher/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
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
                    continue;
                }
            };

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
    // atomically rewritten its credential between polls (e.g. its own token
    // refresh). Re-loading ensures we always have the freshest token rather
    // than a potentially stale in-memory copy.
    //
    // locate+load is sync I/O (a file read, or a `security` subprocess on
    // macOS), so run it on a blocking worker to keep tokio runtime threads
    // free (AGENTS.md §2.1; consistent with the other watcher tasks' sync I/O).
    let (source, creds) = tokio::task::spawn_blocking(|| {
        let source = locate_credentials()?;
        let creds = load_from_source(&source)?;
        Ok::<_, OAuthError>((source, creds))
    })
    .await??;
    let mut oauth = creds.claude_ai_oauth;

    // Pre-flight refresh only for a source we own (a file). The macOS Keychain
    // entry is Claude Code's - read-only (AGENTS.md §3.4): use the token while
    // valid; if it has already expired, surface an actionable error.
    if let Some(path) = source.writable_path() {
        if token_needs_refresh(oauth.expires_at, Utc::now(), REFRESH_MARGIN) {
            tracing::info!("watcher/oauth_poll: token expired/near-expiry - refreshing pre-flight");
            oauth = refresh_and_persist(client, path, oauth).await?;
        }
    } else if token_needs_refresh(oauth.expires_at, Utc::now(), Duration::zero()) {
        return Err(OAuthError::CredentialExpiredReadOnly.into());
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
            // Pre-flight refresh already happened but we still got 401. For a
            // file source, one more refresh + retry (bounded - the retry uses
            // `?` so a second AuthExpired propagates rather than looping). For
            // the read-only Keychain source we can't refresh, so surface the
            // actionable error.
            let Some(path) = source.writable_path() else {
                return Err(OAuthError::CredentialExpiredReadOnly.into());
            };
            tracing::warn!("watcher/oauth_poll: 401 despite pre-flight - one refresh+retry");
            let oauth = refresh_and_persist(client, path, oauth).await?;
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
