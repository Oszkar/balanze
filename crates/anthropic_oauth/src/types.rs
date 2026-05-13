use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Subset of `~/.claude/.credentials.json::claudeAiOauth` that Balanze reads.
/// We never write back to disk; everything is consumed read-only.
#[derive(Debug, Clone, Deserialize)]
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
/// shape (dollar-denominated overage credits with a monthly cap).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraUsage {
    pub is_enabled: bool,
    /// `monthly_limit` × 1_000_000. Raw response uses dollars.
    pub monthly_limit_micro_usd: i64,
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

    #[error("unexpected HTTP status {status} from /api/oauth/usage: {body}")]
    UnexpectedStatus { status: u16, body: String },

    #[error("oauth/usage response shape unexpected: {0}")]
    ResponseShape(String),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}
