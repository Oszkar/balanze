//! Anthropic OAuth refresh-token grant.
//!
//! Per AGENTS.md §4 #3 + §3.4: this crate is the only Anthropic HTTP client
//! and the only toucher of the credentials file. This module performs the
//! grant; `credentials::write_back` persists the result atomically. The
//! refreshed `access_token` / `refresh_token` are secrets — never logged.

use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::types::{OAuthError, RefreshedTokens};

/// Claude Code's public, non-secret OAuth client id (the same identifier the
/// Claude Code CLI uses for its PKCE flow — not a credential). VERIFY via the
/// `#[ignore]` real-endpoint smoke before a release (AGENTS.md §6/§7).
pub const CLAUDE_CODE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Claude Code's OAuth token endpoint. VERIFY via the ignored smoke (above).
pub const CLAUDE_CODE_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

#[derive(Debug, Deserialize)]
struct RawRefreshResponse {
    access_token: String,
    /// Anthropic rotates the refresh token on every grant; required — a
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
    // ambiguous — the server may have already rotated the token while
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

        // Fix 4: reject a non-positive expires_in — a malformed or hostile
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
