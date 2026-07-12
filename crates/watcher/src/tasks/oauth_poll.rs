//! OAuth poll task. Polls `GET /api/oauth/usage` at a configurable interval
//! (default 300s; clamped to a 300s floor at the call site per §3.1). Each tick:
//! 1. Re-locates and re-loads file credentials so Claude Code's atomic rewrites
//!    are observed. A still-valid macOS Keychain credential is cached to avoid
//!    repeated `/usr/bin/security` prompts.
//! 2. Rejects an expired credential with an actionable `claude login` error.
//! 3. Calls `fetch_usage` with `BackoffPolicy::standard()`.
//! 4. Emits `Update(ClaudeOAuth, ...)` to the state coordinator.
//!
//! Every credential source is read-only. Balanze never exchanges Claude Code's
//! rotating refresh token and never writes either credential representation.

use anthropic_oauth::{
    CredentialsClaudeAiOauth, DEFAULT_API_BASE as ANTHROPIC_API_BASE, OAuthError, fetch_usage,
    load_from_source, locate_credentials,
};
use chrono::Utc;
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;
use crate::tasks::get_or_build_client;

/// Spawn the OAuth poll task and return its `JoinHandle`.
///
/// The task runs until the coordinator handle drops (coordinator shuts down).
/// Credential errors on a given tick emit an `Update(ClaudeOAuth, Err(...))`
/// and the task continues - transient failures (network hiccup, 429) are
/// handled by a `BackoffPolicy::standard()` that this task constructs and
/// passes into each `fetch_usage` call (the policy is per-call, not shared
/// across ticks - each tick gets a fresh retry budget).
///
/// `interval_secs` is clamped to a minimum of 300 inside this function so a
/// corrupt or hostile `settings.json` can't drive below the API-politeness
/// floor (AGENTS.md §3.1 - 5 minutes for provider usage/billing endpoints).
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
        // A read-only-source result seeds `keychain_cache` below so the first
        // tick's `poll_once` reuses it instead of immediately re-reading the
        // same macOS Keychain entry a second time.
        let startup_probe = tokio::task::spawn_blocking(|| {
            locate_credentials().and_then(|src| load_from_source(&src).map(|creds| (src, creds)))
        })
        .await;

        let mut keychain_cache: Option<CredentialsClaudeAiOauth> = None;
        match startup_probe {
            Ok(Ok((source, creds))) if source.cache_between_polls() => {
                keychain_cache = Some(creds.claude_ai_oauth);
            }
            Ok(Err(OAuthError::CredentialsMissing { .. })) => {
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
            // A File source isn't cached (re-reading it on the first tick is a
            // cheap fs read), and any other startup error is left for
            // the first tick's fresh `poll_once` read to retry and surface.
            _ => {}
        }

        // First tick fires immediately (interval fires on first `.tick()` call).
        // `Delay` (not the default `Burst`) so a slow tick - `fetch_usage`
        // backing off for up to 10 minutes under `BackoffPolicy::standard()` -
        // can't queue up multiple missed 5-min ticks and fire them
        // back-to-back when the network recovers. That would violate the
        // §3.1 API-politeness floor in exactly the worst conditions.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut client: Option<reqwest::Client> = None;

        loop {
            ticker.tick().await;

            let client = match get_or_build_client(&mut client) {
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

            let result = poll_once(client, &mut keychain_cache).await;
            let update = match result {
                Ok(snapshot) => {
                    // Per-tick success detail: debug, not info (fires every
                    // poll; §3.2). The refresh lifecycle events stay at info.
                    tracing::debug!(
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

/// One poll tick: load a read-only credential, reject it if expired, fetch usage.
///
/// `keychain_cache` carries a still-valid credential from a read-only source
/// (the macOS Keychain) across ticks. Re-locating + re-loading such a source
/// every tick would shell out to `/usr/bin/security` on every poll (every 5
/// minutes), which can re-prompt the user for Keychain access each time -
/// this is the "keeps asking for my password" complaint. A File
/// source is never cached: re-reading it is a plain, cheap fs read (no OS
/// prompt) and is how we notice Claude Code's own atomic rewrites between
/// polls, so it always takes the fresh-read path below.
async fn poll_once(
    client: &reqwest::Client,
    keychain_cache: &mut Option<CredentialsClaudeAiOauth>,
) -> anyhow::Result<anthropic_oauth::ClaudeOAuthSnapshot> {
    let (cacheable, oauth): (bool, CredentialsClaudeAiOauth) = match keychain_cache.take() {
        Some(oauth) if !oauth.is_expired_at(Utc::now()) => (true, oauth),
        _ => {
            // locate+load is sync I/O (a file read, or a `security`
            // subprocess on macOS), so run it on a blocking worker to keep
            // tokio runtime threads free (AGENTS.md §2.1).
            let (source, creds) = tokio::task::spawn_blocking(|| {
                let source = locate_credentials()?;
                let creds = load_from_source(&source)?;
                Ok::<_, OAuthError>((source, creds))
            })
            .await??;
            (source.cache_between_polls(), creds.claude_ai_oauth)
        }
    };

    let policy = backoff::BackoffPolicy::standard();

    if oauth.is_expired_at(Utc::now()) {
        return Err(OAuthError::CredentialExpiredReadOnly.into());
    }

    let result = fetch_usage(
        client,
        ANTHROPIC_API_BASE,
        &oauth.access_token,
        oauth.subscription_type.clone(),
        oauth.rate_limit_tier.clone(),
        &policy,
    )
    .await;

    // Keep caching a read-only credential across ticks unless it was actually
    // rejected (AuthExpired) - a transient network/rate-limit error doesn't
    // mean the credential itself is bad, so don't force a Keychain re-read
    // (and possible re-prompt) over a mere network hiccup.
    if cacheable && !matches!(result, Err(OAuthError::AuthExpired)) {
        *keychain_cache = Some(oauth.clone());
    }

    match result {
        Ok(s) => Ok(s),
        Err(OAuthError::AuthExpired) => Err(OAuthError::CredentialExpiredReadOnly.into()),
        Err(e) => Err(e.into()),
    }
}
