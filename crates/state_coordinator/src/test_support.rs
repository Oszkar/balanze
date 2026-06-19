//! Shared test fixtures. Visible to any module's `#[cfg(test)] mod tests`.

use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot};
use chrono::{DateTime, Duration, TimeZone, Utc};
use claude_parser::{AccountType, DataSource, Provider, UsageEvent};
use openai_client::OpenAiCosts;

pub(crate) fn fixture_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap()
}

pub(crate) fn oauth_snapshot() -> ClaudeOAuthSnapshot {
    ClaudeOAuthSnapshot {
        subscription_type: Some("max".to_string()),
        rate_limit_tier: Some("pro".to_string()),
        org_uuid: Some("uuid-1".to_string()),
        cadences: vec![CadenceBar {
            key: "five_hour".to_string(),
            display_label: "Current 5-hour session".to_string(),
            utilization_percent: 25.0,
            resets_at: fixture_now() + Duration::hours(5),
        }],
        extra_usage: None,
        fetched_at: fixture_now(),
    }
}

/// An OAuth snapshot whose `five_hour` cadence resets at `reset`. Lets a test
/// drive the coordinator's window anchor to a controlled value (e.g. a
/// strictly-future reset that `summarize_window` will actually honor).
pub(crate) fn oauth_snapshot_with_reset(reset: DateTime<Utc>) -> ClaudeOAuthSnapshot {
    ClaudeOAuthSnapshot {
        cadences: vec![CadenceBar {
            key: "five_hour".to_string(),
            display_label: "Current 5-hour session".to_string(),
            utilization_percent: 10.0,
            resets_at: reset,
        }],
        ..oauth_snapshot()
    }
}

pub(crate) fn openai_costs() -> OpenAiCosts {
    OpenAiCosts {
        start_time: fixture_now(),
        end_time: fixture_now(),
        total_micro_usd: 420_000,
        by_line_item: vec![],
        truncated: false,
        fetched_at: fixture_now(),
    }
}

/// A small synthetic deduped event slice for coordinator tests. Two Claude
/// events at known timestamps with models that exist in the bundled price
/// table, so cost derivation produces a non-zero total.
pub(crate) fn sample_events() -> Vec<UsageEvent> {
    vec![
        UsageEvent {
            ts: fixture_now(),
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: Some("msg_sample_1".to_string()),
            request_id: Some("req_sample_1".to_string()),
        },
        UsageEvent {
            ts: fixture_now() + Duration::minutes(5),
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: "claude-haiku-4-5".to_string(),
            input_tokens: 20,
            output_tokens: 10,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: Some("msg_sample_2".to_string()),
            request_id: Some("req_sample_2".to_string()),
        },
    ]
}
