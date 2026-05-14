//! Shared test fixtures. Visible to any module's `#[cfg(test)] mod tests`.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, TimeZone, Utc};
use openai_client::OpenAiCosts;
use window::WindowSummary;

use crate::snapshot::JsonlSnapshot;

pub(crate) fn fixture_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap()
}

pub(crate) fn oauth_snapshot() -> ClaudeOAuthSnapshot {
    ClaudeOAuthSnapshot {
        subscription_type: Some("max".to_string()),
        rate_limit_tier: Some("pro".to_string()),
        org_uuid: Some("uuid-1".to_string()),
        cadences: vec![],
        extra_usage: None,
        fetched_at: fixture_now(),
    }
}

pub(crate) fn jsonl_snapshot() -> JsonlSnapshot {
    JsonlSnapshot {
        files_scanned: 5,
        window: WindowSummary {
            window_start: fixture_now(),
            total_events_in_window: 0,
            total_tokens_in_window: 0,
            recent_burn_tokens_per_min: None,
            by_model: vec![],
        },
    }
}

pub(crate) fn openai_costs() -> OpenAiCosts {
    OpenAiCosts {
        start_time: fixture_now(),
        end_time: fixture_now(),
        total_usd: 0.42,
        by_line_item: vec![],
        truncated: false,
        fetched_at: fixture_now(),
    }
}
