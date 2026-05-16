use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Parsed result from `GET /v1/organization/costs`.
///
/// All monetary fields are USD as f64 (the endpoint returns them as
/// `amount.value` numbers with `amount.currency: "usd"`). We do not convert
/// to micro-USD here — currency math against an external source's values
/// is the caller's concern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiCosts {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    /// Sum of every bucket's every result amount, in USD. The "headline" number.
    pub total_usd: f64,
    /// Per-line-item breakdown, sorted by `amount_usd` descending.
    /// Each entry aggregates across all time buckets returned.
    pub by_line_item: Vec<LineItemCost>,
    /// True if the API said it had more pages and we didn't follow them.
    /// For the standard "this month, daily buckets" query this should
    /// always be false; if true, the totals are partial and the caller
    /// should consider paginating.
    pub truncated: bool,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineItemCost {
    /// e.g. `"gpt-5"`, `"o1-mini"`, or `"unknown"` when the API returned null.
    pub line_item: String,
    pub amount_usd: f64,
}

#[derive(Debug, Error)]
pub enum OpenAiError {
    /// HTTP 401 — admin key invalid or revoked.
    #[error("OpenAI rejected the admin key (HTTP 401): {body}")]
    AuthInvalid { body: String },

    /// HTTP 403 — key lacks the admin scope (project/service-account keys hit this).
    #[error("HTTP 403 from organization/costs. This endpoint requires an admin API key (`sk-admin-…`); project keys and service-account keys cannot read organization billing. Generate an admin key at https://platform.openai.com/settings/organization/admin-keys and try again. Server said: {body}")]
    InsufficientScope { body: String },

    #[error("unexpected HTTP status {status} from organization/costs: {body}")]
    UnexpectedStatus { status: u16, body: String },

    #[error("rate limited by OpenAI (HTTP 429)")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },

    #[error("organization/costs response shape unexpected: {0}")]
    ResponseShape(String),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}
