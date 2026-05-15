use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Subset of `~/.claude/.credentials.json::claudeAiOauth` that Balanze reads.
/// We never write back to disk; everything is consumed read-only.
///
/// `Debug` is hand-written (NOT derived) so `access_token` / `refresh_token`
/// cannot leak via a stray `{:?}` / `tracing::debug!(?creds)`. Per AGENTS.md
/// §3.4 these are secrets identical to OpenAI keys — never logged at any
/// level. `Credentials` keeps a derived `Debug`; it delegates to this impl,
/// so the wrapper is safe too.
#[derive(Clone, Deserialize)]
pub struct CredentialsClaudeAiOauth {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "refreshToken")]
    pub refresh_token: Option<String>,
    /// Milliseconds since Unix epoch when the access token expires.
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
    #[serde(rename = "subscriptionType")]
    pub subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier")]
    pub rate_limit_tier: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for CredentialsClaudeAiOauth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialsClaudeAiOauth")
            .field("access_token", &"<redacted>")
            // Reveal presence (Some/None) but never the value — useful for
            // diagnosing "no refresh token" without leaking the token.
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("subscription_type", &self.subscription_type)
            .field("rate_limit_tier", &self.rate_limit_tier)
            .field("scopes", &self.scopes)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    pub claude_ai_oauth: CredentialsClaudeAiOauth,
}

/// One rolling-window meter as exposed by `/api/oauth/usage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CadenceBar {
    /// Raw key from Anthropic's response (e.g. "five_hour", "seven_day_sonnet").
    /// Internal codenames (omelette, tangelo, etc.) are preserved verbatim.
    pub key: String,
    /// Human-friendly label. Known keys map to curated strings; unknown keys
    /// titlecase the raw key so new Anthropic additions still render.
    pub display_label: String,
    /// 0.0 to 100.0. Anthropic returns percentages, not 0.0..1.0 fractions.
    pub utilization_percent: f32,
    pub resets_at: DateTime<Utc>,
}

/// The `extra_usage` block — separate from cadence bars because it has different
/// shape (a numeric counter with a cap and a percentage).
///
/// **The semantic of this block is currently UNKNOWN and the values should not
/// be trusted as dollar amounts.** Investigation as of May 2026:
///
/// - The visible claude.ai/settings/usage UI shows "$4.67 spent this month / $20
///   monthly spend limit / 23% used" for the user's account.
/// - OAuth `/api/oauth/usage` returns `{monthly_limit: 2000, used_credits: 1763,
///   utilization: 88.15, currency: "USD"}` for the same account at the same time.
/// - hamed-elfayome's Claude Usage Tracker (Electron, macOS) renders the OAuth
///   numbers as `$17.63 / $20.00` — i.e. it treats the raw values as cents.
/// - Treating as cents reconciles `monthly_limit = 2000 → $20.00` with the UI's
///   `$20 monthly spend limit`, but `used_credits = 1763 → $17.63` does NOT match
///   the UI's `$4.67 spent`. Interpreting `used_credits` as "remaining" also
///   doesn't reconcile cleanly ($20 - $17.63 = $2.37, still $2.30 off from $4.67).
///
/// Possibilities (none verified):
/// - `used_credits` is a lifetime spend tally rather than current-month spend.
/// - `used_credits` is a different metric (committed balance, reserved credits).
/// - The visible UI's "$4.67 spent" comes from a different endpoint we haven't
///   found; a claude.ai/settings/usage HAR capture would identify it.
///
/// Until the semantic is resolved, the CLI's pretty output suppresses the
/// extra_usage block. The data is still parsed and exposed in --json output for
/// diagnostic purposes. Units are stored as micro-USD assuming cents-input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraUsage {
    pub is_enabled: bool,
    /// Raw `monthly_limit` is in cents (assumed). We store as micro-USD (× 10_000).
    pub monthly_limit_micro_usd: i64,
    /// Raw `used_credits` is in cents (assumed). Semantic unclear — see struct doc.
    pub used_credits_micro_usd: i64,
    pub utilization_percent: f32,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeOAuthSnapshot {
    pub cadences: Vec<CadenceBar>,
    pub extra_usage: Option<ExtraUsage>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
    /// From the `anthropic-organization-id` response header — identifies the
    /// Claude consumer subscription org (distinct from any platform.claude.com
    /// API org for the same user).
    pub org_uuid: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

impl ClaudeOAuthSnapshot {
    /// The Anthropic 5-hour rolling-window reset timestamp, if the OAuth
    /// `/api/oauth/usage` response carried a `five_hour` cadence. Centralizes
    /// the `"five_hour"` raw-key knowledge in the crate that owns the OAuth
    /// schema (AGENTS.md §4 #1/#3) so glue callers don't encode the wire key.
    pub fn five_hour_reset(&self) -> Option<DateTime<Utc>> {
        self.cadences
            .iter()
            .find(|c| c.key == "five_hour")
            .map(|c| c.resets_at)
    }
}

/// Result of a successful refresh-token grant. Hand-written `Debug` (NOT
/// derived) so the tokens cannot leak via `{:?}` — identical discipline to
/// `CredentialsClaudeAiOauth` (AGENTS.md §3.4).
#[derive(Clone)]
pub struct RefreshedTokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Milliseconds since Unix epoch (matches `CredentialsClaudeAiOauth::expires_at`).
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for RefreshedTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshedTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("credentials file not found (looked at {searched:?})")]
    CredentialsMissing { searched: Vec<std::path::PathBuf> },

    #[error("credentials file at {path:?} is malformed: {reason}")]
    CredentialsMalformed {
        path: std::path::PathBuf,
        reason: String,
    },

    #[error("io error reading {path:?}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("oauth bearer expired or invalid (HTTP 401) — user must re-run `claude login` or refresh token must be exchanged")]
    AuthExpired,

    #[error("oauth refresh-token grant failed (HTTP {status}): {body}")]
    RefreshFailed { status: u16, body: String },

    #[error(
        "credentials file has no refreshToken — cannot refresh; user must re-run `claude login`"
    )]
    RefreshTokenMissing,

    #[error("unexpected HTTP status {status} from /api/oauth/usage: {body}")]
    UnexpectedStatus { status: u16, body: String },

    #[error("oauth/usage response shape unexpected: {0}")]
    ResponseShape(String),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_hour_reset_returns_the_five_hour_cadence_timestamp() {
        let ts = chrono::DateTime::parse_from_rfc3339("2026-05-15T18:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let other = chrono::DateTime::parse_from_rfc3339("2026-05-20T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let snap = ClaudeOAuthSnapshot {
            cadences: vec![
                CadenceBar {
                    key: "seven_day".to_string(),
                    display_label: "All models (7 days)".to_string(),
                    utilization_percent: 10.0,
                    resets_at: other,
                },
                CadenceBar {
                    key: "five_hour".to_string(),
                    display_label: "Current 5-hour session".to_string(),
                    utilization_percent: 42.0,
                    resets_at: ts,
                },
            ],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: ts,
        };
        assert_eq!(snap.five_hour_reset(), Some(ts));
    }

    #[test]
    fn five_hour_reset_is_none_when_absent() {
        let ts = chrono::DateTime::parse_from_rfc3339("2026-05-15T18:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let snap = ClaudeOAuthSnapshot {
            cadences: vec![CadenceBar {
                key: "seven_day".to_string(),
                display_label: "All models (7 days)".to_string(),
                utilization_percent: 10.0,
                resets_at: ts,
            }],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: ts,
        };
        assert_eq!(snap.five_hour_reset(), None);
    }
}
