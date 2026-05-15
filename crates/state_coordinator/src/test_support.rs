//! Shared test fixtures. Visible to any module's `#[cfg(test)] mod tests`.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, TimeZone, Utc};
use claude_cost::Cost;
use codex_local::{CodexQuotaSnapshot, RateLimitWindow};
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

pub(crate) fn anthropic_api_cost() -> Cost {
    Cost {
        per_model: vec![],
        total_micro_usd: 12_345_678,
        skipped_models: vec![],
        total_event_count: 0,
        unparsed_event_count: 0,
    }
}

pub(crate) fn codex_quota() -> CodexQuotaSnapshot {
    CodexQuotaSnapshot {
        observed_at: fixture_now(),
        session_id: "00000000-0000-7000-8000-000000000001".to_string(),
        primary: RateLimitWindow {
            used_percent: 3.0,
            window_duration_minutes: 10_080,
            resets_at: fixture_now(),
        },
        secondary: None,
        plan_type: "go".to_string(),
        rate_limit_reached: false,
    }
}
