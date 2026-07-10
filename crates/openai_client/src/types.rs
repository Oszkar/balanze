use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Parsed result from `GET /v1/organization/costs`.
///
/// Monetary fields are `i64` micro-USD (AGENTS.md §2.1). The endpoint returns
/// USD numbers (`amount.value`, `amount.currency: "usd"`); we convert each at
/// the parse boundary so every money cell in the `Snapshot` is the same kind of
/// integer and never has to be summed or threshold-compared as `f64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiCosts {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    /// Sum of every bucket's every result amount, in micro-USD. The "headline" number.
    pub total_micro_usd: i64,
    /// Per-line-item breakdown, sorted by `amount_micro_usd` descending.
    /// Each entry aggregates across all time buckets returned.
    pub by_line_item: Vec<LineItemCost>,
    /// True if the API said it had more pages and we didn't follow them.
    /// For the standard "this month, daily buckets" query this should
    /// always be false; if true, the totals are partial and the caller
    /// should consider paginating.
    pub truncated: bool,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineItemCost {
    /// e.g. `"gpt-5"`, `"o1-mini"`, or `"unknown"` when the API returned null.
    pub line_item: String,
    pub amount_micro_usd: i64,
}

#[derive(Debug, Error)]
pub enum OpenAiError {
    /// HTTP 401 - admin key invalid or revoked.
    #[error("OpenAI rejected the admin key (HTTP 401): {body}")]
    AuthInvalid { body: String },

    /// HTTP 403 - key lacks the admin scope (project/service-account keys hit this).
    #[error(
        "HTTP 403 from organization/costs. This endpoint requires an admin API key (`sk-admin-...`); project keys and service-account keys cannot read organization billing. Generate an admin key at https://platform.openai.com/settings/organization/admin-keys and try again. Server said: {body}"
    )]
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

impl OpenAiError {
    /// A user-facing remediation hint for the two admin-key auth failures
    /// (401 invalid/revoked key, 403 wrong scope). Shared by the CLI `status`
    /// path and the watcher poller so their guidance cannot drift. Returns
    /// `None` for every other variant, which callers format with the `Display`
    /// impl instead.
    pub fn admin_key_hint(&self) -> Option<&'static str> {
        match self {
            OpenAiError::AuthInvalid { .. } => Some(
                "OpenAI admin key rejected (HTTP 401). Run `balanze-cli set-openai-key` with a fresh `sk-admin-...` key.",
            ),
            OpenAiError::InsufficientScope { .. } => Some(
                "OpenAI returned 403. organization/costs requires an admin API key (`sk-admin-...`), not a project or service-account key. Generate one at https://platform.openai.com/settings/organization/admin-keys.",
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_key_hint_covers_the_two_auth_failures_only() {
        let h401 = OpenAiError::AuthInvalid { body: "x".into() }
            .admin_key_hint()
            .expect("401 has a hint");
        assert!(h401.contains("HTTP 401") && h401.contains("set-openai-key"));

        let h403 = OpenAiError::InsufficientScope { body: "x".into() }
            .admin_key_hint()
            .expect("403 has a hint");
        assert!(h403.contains("403") && h403.contains("admin-keys"));

        // Non-auth variants carry no hint; callers use Display for those.
        assert!(
            OpenAiError::ResponseShape("x".into())
                .admin_key_hint()
                .is_none()
        );
        assert!(
            OpenAiError::RateLimited { retry_after: None }
                .admin_key_hint()
                .is_none()
        );
        assert!(
            OpenAiError::UnexpectedStatus {
                status: 500,
                body: "x".into()
            }
            .admin_key_hint()
            .is_none()
        );
    }
}
