use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Parsed result from `GET /v1/dashboard/billing/credit_grants`.
///
/// All monetary fields are USD as f64 (the endpoint returns them that way).
/// We do NOT convert to micro-USD here — currency math against an external
/// source's values is the caller's concern. If the caller needs integer
/// math, multiply at the call site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditGrants {
    pub total_granted_usd: f64,
    pub total_used_usd: f64,
    pub total_available_usd: f64,
    /// Earliest future grant-expiry across all grants. `None` if every grant
    /// has already expired (in which case `total_available_usd` will be 0).
    pub next_grant_expiry: Option<DateTime<Utc>>,
    pub grants: Vec<Grant>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub grant_amount_usd: f64,
    pub used_amount_usd: f64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum OpenAiError {
    /// HTTP 401 — API key invalid or revoked. User must regenerate or
    /// re-paste the key.
    #[error("OpenAI rejected the API key (HTTP 401): {body}")]
    AuthExpired { body: String },

    /// HTTP 403. The credit_grants endpoint is restricted to legacy/user
    /// keys; project keys (`sk-proj-…`) return 403. Most common failure
    /// mode for new users; surface with a clear hint.
    #[error("HTTP 403 from credit_grants. The endpoint requires a legacy/user API key; project keys (`sk-proj-…`) do not have billing access. Generate a legacy key in your OpenAI account settings and try again. Server said: {body}")]
    ForbiddenProjectKey { body: String },

    #[error("unexpected HTTP status {status} from credit_grants: {body}")]
    UnexpectedStatus { status: u16, body: String },

    #[error("credit_grants response shape unexpected: {0}")]
    ResponseShape(String),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}
