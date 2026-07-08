//! Anthropic OAuth refresh-token grant.
//!
//! Per AGENTS.md §4 #3 + §3.4: this crate is the only Anthropic HTTP client
//! and the only toucher of the credentials file. This module performs the
//! grant; `credentials::write_back` persists the result atomically. The
//! refreshed `access_token` / `refresh_token` are secrets - never logged.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::credentials::{WriteBack, write_back};
use crate::types::{CredentialsClaudeAiOauth, OAuthError, RefreshedTokens};

/// Claude Code's public, non-secret OAuth client id (the same identifier the
/// Claude Code CLI uses for its PKCE flow - not a credential). VERIFY via the
/// `#[ignore]` real-endpoint smoke before a release (AGENTS.md §6/§7).
pub const CLAUDE_CODE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Claude Code's OAuth token endpoint. VERIFY via the ignored smoke (above).
pub const CLAUDE_CODE_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// Refresh proactively if the access token is expired or expires within this
/// window. AGENTS.md §3.1/§3.4: both the one-shot CLI and the watcher pre-flight
/// refresh a writable credential this far ahead of expiry.
pub const REFRESH_MARGIN: Duration = Duration::seconds(300);

/// Pure: `true` if `expires_at_ms` is in the past or within `margin` of `now`.
///
/// `saturating_sub` guards against underflow on a pathological/hostile
/// `expires_at_ms` near `i64::MIN`, which would otherwise panic in debug builds.
pub fn token_needs_refresh(expires_at_ms: i64, now: DateTime<Utc>, margin: Duration) -> bool {
    now.timestamp_millis() >= expires_at_ms.saturating_sub(margin.num_milliseconds())
}

#[derive(Debug, Deserialize)]
struct RawRefreshResponse {
    access_token: String,
    /// Anthropic rotates the refresh token on every grant; required - a
    /// response without it would strand us (can't refresh again).
    refresh_token: String,
    /// Seconds until the new access token expires.
    expires_in: i64,
}

/// Exchange a refresh token for a fresh access token.
///
/// `token_url` / `client_id` are the constants above (tests override
/// `token_url` to point at wiremock). `now_ms` is injected so the expiry math
/// is testable without a wall clock. Non-200 → `RefreshFailed` (body
/// redacted); transport error → `Network`. Nothing here is ever logged.
///
/// `policy` controls backoff+retry for transient errors (429, 5xx, network).
/// Pass `BackoffPolicy::fail_fast()` from one-shot CLI callers.
pub async fn refresh_access_token(
    client: &Client,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
    now_ms: i64,
    policy: &backoff::BackoffPolicy,
) -> Result<RefreshedTokens, OAuthError> {
    // The refresh-token grant is a token-ROTATING POST: Anthropic issues a
    // new refresh token on every grant. Only HTTP 429 is provably safe to
    // retry (rate-limited ⇒ rejected before the grant is processed ⇒ the
    // old refresh token is untouched). A 5xx or transport timeout is
    // ambiguous - the server may have already rotated the token while
    // failing to respond, so a retry would replay a consumed token and
    // strand the user. Fail fast on everything except 429; the caller
    // re-derives from a fresh `claude login`. (Contrast `fetch_usage`,
    // an idempotent GET, which DOES retry 5xx/transport.)
    let classify = |e: &OAuthError| match e {
        OAuthError::RateLimited { retry_after } => backoff::RetryDecision::RetryAfter(*retry_after),
        _ => backoff::RetryDecision::DoNotRetry,
    };

    backoff::retry(policy, classify, || async {
        let resp = client
            .post(token_url)
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": client_id,
            }))
            .header("Accept", "application/json")
            .send()
            .await?;

        let status = resp.status();

        // Read Retry-After BEFORE consuming the body (headers are unavailable
        // after `resp.text()` takes ownership).
        let retry_after = crate::client::parse_retry_after(resp.headers());

        let body = resp.text().await?;

        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(OAuthError::RateLimited { retry_after });
        }

        if status != StatusCode::OK {
            return Err(OAuthError::RefreshFailed {
                status: status.as_u16(),
                body: crate::client::redact_for_display(&body),
            });
        }

        let raw: RawRefreshResponse = serde_json::from_str(&body).map_err(|e| {
            OAuthError::ResponseShape(format!(
                "refresh response: {}",
                crate::client::redact_for_display(&e.to_string())
            ))
        })?;

        // Fix 4: reject a non-positive expires_in - a malformed or hostile
        // response must not yield an already-expired credential that would
        // trigger confusing immediate retry behavior.
        if raw.expires_in <= 0 {
            return Err(OAuthError::ResponseShape(
                "refresh response: non-positive expires_in".into(),
            ));
        }

        Ok(RefreshedTokens {
            access_token: raw.access_token,
            refresh_token: raw.refresh_token,
            expires_at_ms: now_ms.saturating_add(raw.expires_in.saturating_mul(1000)),
        })
    })
    .await
}

/// Refresh the bearer token and best-effort persist it back to the credential
/// file Balanze owns (AGENTS.md §3.4 - the only OAuth write path). A skipped or
/// failed write is non-fatal as long as we still hold a usable in-memory token.
///
/// `policy` is the caller's backoff: `BackoffPolicy::fail_fast()` from the
/// one-shot CLI, `BackoffPolicy::standard()` from the watcher. The refreshed
/// `access_token` / `refresh_token` are secrets and are never logged.
pub async fn refresh_and_persist(
    client: &Client,
    path: &Path,
    oauth: CredentialsClaudeAiOauth,
    policy: &backoff::BackoffPolicy,
) -> Result<CredentialsClaudeAiOauth, OAuthError> {
    let rt = oauth
        .refresh_token
        .as_deref()
        .ok_or(OAuthError::RefreshTokenMissing)?;
    let refreshed = refresh_access_token(
        client,
        CLAUDE_CODE_TOKEN_URL,
        CLAUDE_CODE_CLIENT_ID,
        rt,
        Utc::now().timestamp_millis(),
        policy,
    )
    .await?;
    match write_back(path, &refreshed) {
        Ok(WriteBack::Written) => tracing::info!("oauth: refreshed bearer, wrote back"),
        Ok(WriteBack::SkippedDiskNewer) => {
            tracing::info!("oauth: refreshed bearer; on-disk copy already newer, kept disk")
        }
        Err(e) => tracing::warn!("oauth: refresh ok but write-back failed (non-fatal): {e}"),
    }
    let mut next = oauth;
    next.access_token = refreshed.access_token;
    next.refresh_token = Some(refreshed.refresh_token);
    next.expires_at = refreshed.expires_at_ms;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::token_needs_refresh;
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn token_needs_refresh_logic() {
        let now = Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();
        let margin = Duration::seconds(300);
        let now_ms = now.timestamp_millis();
        assert!(token_needs_refresh(now_ms - 1, now, margin));
        assert!(token_needs_refresh(now_ms + 200_000, now, margin));
        assert!(!token_needs_refresh(now_ms + 3_600_000, now, margin));
        // Boundary: token expiring exactly `margin` from now → refresh now.
        assert!(token_needs_refresh(
            now_ms + margin.num_milliseconds(),
            now,
            margin
        ));
        // A pathological/hostile expires_at near i64::MIN must not panic and
        // must return true (absurdly-past expiry → needs refresh).
        assert!(token_needs_refresh(i64::MIN, now, margin));
    }
}
