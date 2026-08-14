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

/// Safe provider failure classification persisted by the shared Costs gate.
/// Response bodies and transport details never cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredFailureKind {
    AuthInvalid,
    InsufficientScope,
    RateLimited,
    UnexpectedStatus(u16),
    Network,
    ResponseShape,
}

impl StoredFailureKind {
    pub fn admin_key_hint(self) -> Option<&'static str> {
        match self {
            Self::AuthInvalid => Some(
                "OpenAI admin key rejected (HTTP 401). Run `balanze-cli set-openai-key` with a fresh `sk-admin-...` key.",
            ),
            Self::InsufficientScope => Some(
                "OpenAI returned 403. organization/costs requires an admin API key (`sk-admin-...`), not a project or service-account key. Generate one at https://platform.openai.com/settings/organization/admin-keys.",
            ),
            _ => None,
        }
    }

    pub fn is_retryable(self) -> bool {
        !matches!(self, Self::AuthInvalid | Self::InsufficientScope)
    }
}

impl From<&OpenAiError> for StoredFailureKind {
    fn from(error: &OpenAiError) -> Self {
        match error {
            OpenAiError::AuthInvalid { .. } => Self::AuthInvalid,
            OpenAiError::InsufficientScope { .. } => Self::InsufficientScope,
            OpenAiError::RateLimited { .. } => Self::RateLimited,
            OpenAiError::UnexpectedStatus { status, .. } => Self::UnexpectedStatus(*status),
            OpenAiError::Network(_) => Self::Network,
            OpenAiError::ResponseShape(_) => Self::ResponseShape,
        }
    }
}

/// Cached value exposed to callers when the shared gate defers a request.
/// Full and legacy headline-only values are mutually exclusive on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CachedOpenAiCosts {
    Full {
        costs: OpenAiCosts,
    },
    LegacyHeadline {
        total_micro_usd: i64,
        fetched_at: DateTime<Utc>,
    },
}

impl CachedOpenAiCosts {
    pub fn total_micro_usd(&self) -> i64 {
        match self {
            Self::Full { costs } => costs.total_micro_usd,
            Self::LegacyHeadline {
                total_micro_usd, ..
            } => *total_micro_usd,
        }
    }

    pub fn fetched_at(&self) -> DateTime<Utc> {
        match self {
            Self::Full { costs } => costs.fetched_at,
            Self::LegacyHeadline { fetched_at, .. } => *fetched_at,
        }
    }

    pub fn full(&self) -> Option<&OpenAiCosts> {
        match self {
            Self::Full { costs } => Some(costs),
            Self::LegacyHeadline { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDeferredReason {
    RecentAttempt,
    InFlight,
    PriorMonth,
    LegacyHeadline,
    LeaseBusy,
    Capacity,
}

/// Error returned by the provider-owned Costs gate. It retains a safe cached
/// value for presentation callers without exposing credentials or response bodies.
#[derive(Debug, Error)]
pub enum CostsGateError {
    #[error("{source}")]
    Provider {
        #[source]
        source: OpenAiError,
        cached: Option<CachedOpenAiCosts>,
    },

    #[error(
        "OpenAI Costs request deferred by the shared 300-second gate ({reason:?}); retry in about {retry_after_secs}s"
    )]
    Deferred {
        reason: GateDeferredReason,
        retry_after_secs: u64,
        failure: Option<StoredFailureKind>,
        cached: Option<CachedOpenAiCosts>,
    },

    #[error("OpenAI Costs gate unavailable: {message}")]
    Unavailable {
        message: String,
        cached: Option<CachedOpenAiCosts>,
    },
}

impl CostsGateError {
    pub fn cached_total_micro_usd(&self) -> Option<i64> {
        self.cached().map(CachedOpenAiCosts::total_micro_usd)
    }

    pub fn cached_full_costs(&self) -> Option<&OpenAiCosts> {
        self.cached().and_then(CachedOpenAiCosts::full)
    }

    pub fn failure_kind(&self) -> Option<StoredFailureKind> {
        match self {
            Self::Provider { source, .. } => Some(source.into()),
            Self::Deferred { failure, .. } => *failure,
            Self::Unavailable { .. } => None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        self.failure_kind()
            .is_none_or(StoredFailureKind::is_retryable)
    }

    pub fn admin_key_hint(&self) -> Option<&'static str> {
        match self {
            Self::Provider { source, .. } => source.admin_key_hint(),
            _ => self
                .failure_kind()
                .and_then(StoredFailureKind::admin_key_hint),
        }
    }

    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Deferred {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            _ => None,
        }
    }

    fn cached(&self) -> Option<&CachedOpenAiCosts> {
        match self {
            Self::Provider { cached, .. }
            | Self::Deferred { cached, .. }
            | Self::Unavailable { cached, .. } => cached.as_ref(),
        }
    }
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
