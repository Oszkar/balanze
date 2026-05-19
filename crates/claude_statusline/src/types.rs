use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One server-authoritative subscription window from the statusLine feed.
/// `anthropic_oauth::CadenceBar`'s analogous field is `utilization_percent`;
/// `RateWindow` uses the shorter `used_percent` and `resets_at: DateTime<Utc>`.
/// Track E aligns the two sources (a small field-name mapping step).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    pub used_percent: f32,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    pub five_hour: Option<RateWindow>,
    pub seven_day: Option<RateWindow>,
}

/// Parsed statusLine payload. `None` fields = "not present in this payload"
/// (e.g. `rate_limits` is Pro/Max-only and only after the first API
/// response). `session_cost_micro_usd` is a Claude-side SESSION ESTIMATE
/// (i64 micro-USD, AGENTS.md §2.1) — a distinct cost tier, never conflated
/// with the JSONL list-price estimate or the real `extra_usage` overage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatuslineSnapshot {
    pub rate_limits: Option<RateLimits>,
    pub session_cost_micro_usd: Option<i64>,
    pub claude_code_version: Option<String>,
}
