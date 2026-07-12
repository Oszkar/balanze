//! OAuth poll task. Polls `GET /api/oauth/usage` at a configurable interval
//! (default 300s; clamped to a 300s floor at the call site per §3.1). Each tick:
//! 1. Re-locates and re-loads file credentials so Claude Code's atomic rewrites
//!    are observed. A still-valid macOS Keychain credential is cached to avoid
//!    repeated `/usr/bin/security` prompts.
//! 2. Rejects an expired credential with an actionable `claude login` error.
//! 3. On HTTP 401, re-reads once and retries only when Claude Code rotated the
//!    bearer during the poll.
//! 4. Holds rejected/expired Keychain state for a bounded cooldown so a broken
//!    credential cannot prompt every tick.
//! 5. Emits `Update(ClaudeOAuth, ...)` to the state coordinator.
//!
//! Every credential source is read-only. Balanze never exchanges Claude Code's
//! rotating refresh token and never writes either credential representation.

use std::time::{Duration, Instant};

use anthropic_oauth::{
    CredentialsClaudeAiOauth, DEFAULT_API_BASE as ANTHROPIC_API_BASE, OAuthError, fetch_usage,
    load_from_source, locate_credentials,
};
use chrono::Utc;
use state_coordinator::{
    Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg, WatcherGeneration,
};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;
use crate::tasks::get_or_build_client;

// An invalid Keychain credential is terminal for six normal poll intervals.
// This keeps recovery bounded without bringing back a password prompt every
// five minutes. File credentials are never put in this cache.
const KEYCHAIN_RECHECK_COOLDOWN: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
enum KeychainCache {
    #[default]
    Empty,
    Ready(CredentialsClaudeAiOauth),
    RecheckAfter(Instant),
}

impl KeychainCache {
    fn store_ready(&mut self, oauth: CredentialsClaudeAiOauth) {
        *self = Self::Ready(oauth);
    }

    fn mark_terminal(&mut self, now: Instant) {
        *self = Self::RecheckAfter(now + KEYCHAIN_RECHECK_COOLDOWN);
    }

    fn take_ready(
        &mut self,
        now: Instant,
        wall_now: chrono::DateTime<Utc>,
    ) -> Result<Option<CredentialsClaudeAiOauth>, OAuthError> {
        match std::mem::take(self) {
            Self::Empty => Ok(None),
            Self::Ready(oauth) if oauth.is_expired_at(wall_now) => {
                self.mark_terminal(now);
                Err(OAuthError::CredentialExpiredReadOnly)
            }
            Self::Ready(oauth) => Ok(Some(oauth)),
            Self::RecheckAfter(recheck_at) if now < recheck_at => {
                *self = Self::RecheckAfter(recheck_at);
                Err(OAuthError::CredentialExpiredReadOnly)
            }
            Self::RecheckAfter(_) => Ok(None),
        }
    }
}

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
    generation: WatcherGeneration,
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

        let mut keychain_cache = KeychainCache::default();
        match startup_probe {
            Ok(Ok((source, creds))) if source.cache_between_polls() => {
                keychain_cache.store_ready(creds.claude_ai_oauth);
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
                        generation,
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
                            generation,
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
                    // Per-tick success detail: debug, not info (fires every poll; §3.2).
                    tracing::debug!(
                        "watcher/oauth_poll: fetched {} cadence bars",
                        snapshot.cadences.len()
                    );
                    SourceUpdate {
                        generation,
                        source: Source::ClaudeOAuth,
                        result: Ok(SourcePartial::ClaudeOAuth(snapshot)),
                    }
                }
                Err(e) => {
                    tracing::warn!("watcher/oauth_poll: fetch error: {e}");
                    SourceUpdate {
                        generation,
                        source: Source::ClaudeOAuth,
                        result: Err(format!("{e}")),
                    }
                }
            };
            let _ = coord.send(StateMsg::Update(update)).await;
        }
    })
}

/// One poll tick: load a read-only credential, reject it if expired, fetch
/// usage, and re-read once after a 401 to observe a concurrent Claude Code
/// rotation. Keychain authentication failures enter a bounded terminal state;
/// file sources continue to re-read every tick.
async fn poll_once(
    client: &reqwest::Client,
    keychain_cache: &mut KeychainCache,
) -> anyhow::Result<anthropic_oauth::ClaudeOAuthSnapshot> {
    let now = Instant::now();
    let (cacheable, oauth) = match keychain_cache.take_ready(now, Utc::now())? {
        Some(oauth) => (true, oauth),
        None => load_read_only_credential().await?,
    };

    let policy = backoff::BackoffPolicy::standard();

    if oauth.is_expired_at(Utc::now()) {
        if cacheable {
            keychain_cache.mark_terminal(now);
        }
        return Err(OAuthError::CredentialExpiredReadOnly.into());
    }

    match fetch_for_credential(client, &oauth, &policy).await {
        Ok(snapshot) => {
            if cacheable {
                keychain_cache.store_ready(oauth);
            }
            Ok(snapshot)
        }
        Err(OAuthError::AuthExpired) => {
            retry_after_credential_reread(client, keychain_cache, cacheable, &oauth, &policy).await
        }
        Err(e) => {
            // A transient provider/network failure does not invalidate a
            // Keychain credential or justify another access prompt next tick.
            if cacheable {
                keychain_cache.store_ready(oauth);
            }
            Err(e.into())
        }
    }
}

async fn load_read_only_credential() -> anyhow::Result<(bool, CredentialsClaudeAiOauth)> {
    tokio::task::spawn_blocking(|| {
        let source = locate_credentials()?;
        let creds = load_from_source(&source)?;
        Ok::<_, OAuthError>((source.cache_between_polls(), creds.claude_ai_oauth))
    })
    .await?
    .map_err(Into::into)
}

async fn fetch_for_credential(
    client: &reqwest::Client,
    oauth: &CredentialsClaudeAiOauth,
    policy: &backoff::BackoffPolicy,
) -> Result<anthropic_oauth::ClaudeOAuthSnapshot, OAuthError> {
    fetch_usage(
        client,
        ANTHROPIC_API_BASE,
        &oauth.access_token,
        oauth.subscription_type.clone(),
        oauth.rate_limit_tier.clone(),
        policy,
    )
    .await
}

async fn retry_after_credential_reread(
    client: &reqwest::Client,
    keychain_cache: &mut KeychainCache,
    prior_cacheable: bool,
    prior: &CredentialsClaudeAiOauth,
    policy: &backoff::BackoffPolicy,
) -> anyhow::Result<anthropic_oauth::ClaudeOAuthSnapshot> {
    let now = Instant::now();
    let (cacheable, current) = match load_read_only_credential().await {
        Ok(loaded) => loaded,
        Err(e) => {
            if prior_cacheable {
                keychain_cache.mark_terminal(now);
            }
            return Err(e);
        }
    };

    // A re-read closes the race with Claude Code's atomic rotation. If the
    // bearer did not change, another request cannot succeed and would only add
    // provider traffic.
    if current.is_expired_at(Utc::now()) || current.access_token == prior.access_token {
        if cacheable {
            keychain_cache.mark_terminal(now);
        }
        return Err(OAuthError::CredentialExpiredReadOnly.into());
    }

    match fetch_for_credential(client, &current, policy).await {
        Ok(snapshot) => {
            if cacheable {
                keychain_cache.store_ready(current);
            }
            Ok(snapshot)
        }
        Err(OAuthError::AuthExpired) => {
            if cacheable {
                keychain_cache.mark_terminal(now);
            }
            Err(OAuthError::CredentialExpiredReadOnly.into())
        }
        Err(e) => {
            if cacheable {
                keychain_cache.store_ready(current);
            }
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn credential(expires_at: i64) -> CredentialsClaudeAiOauth {
        CredentialsClaudeAiOauth {
            access_token: "secret".into(),
            refresh_token: None,
            expires_at,
            subscription_type: None,
            rate_limit_tier: None,
            scopes: Vec::new(),
        }
    }

    #[test]
    fn valid_keychain_credential_is_reused_without_recheck() {
        let wall_now = Utc.with_ymd_and_hms(2026, 7, 12, 12, 0, 0).unwrap();
        let mut cache = KeychainCache::default();
        cache.store_ready(credential(wall_now.timestamp_millis() + 60_000));

        assert!(
            cache
                .take_ready(Instant::now(), wall_now)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn expired_keychain_credential_enters_terminal_cooldown() {
        let wall_now = Utc.with_ymd_and_hms(2026, 7, 12, 12, 0, 0).unwrap();
        let now = Instant::now();
        let mut cache = KeychainCache::default();
        cache.store_ready(credential(wall_now.timestamp_millis()));

        assert!(matches!(
            cache.take_ready(now, wall_now),
            Err(OAuthError::CredentialExpiredReadOnly)
        ));
        assert!(matches!(
            cache.take_ready(now + Duration::from_secs(5 * 60), wall_now),
            Err(OAuthError::CredentialExpiredReadOnly)
        ));
    }

    #[test]
    fn terminal_keychain_state_rechecks_after_bounded_cooldown() {
        let wall_now = Utc.with_ymd_and_hms(2026, 7, 12, 12, 0, 0).unwrap();
        let now = Instant::now();
        let mut cache = KeychainCache::default();
        cache.mark_terminal(now);

        assert!(
            cache
                .take_ready(now + KEYCHAIN_RECHECK_COOLDOWN, wall_now)
                .unwrap()
                .is_none()
        );
    }
}
