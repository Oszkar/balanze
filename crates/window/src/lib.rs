//! Pure rolling-window math over `UsageEvent` slices.
//!
//! Per AGENTS.md §4 #2, this crate is I/O-free and synchronous: it consumes
//! event slices (from `claude_parser`) and produces a typed summary.
//! No `tokio::spawn`, no `reqwest`, no logging above `debug`. Orchestration
//! (file scanning, error fan-out, display) belongs in the caller.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use claude_parser::UsageEvent;
use serde::{Deserialize, Serialize};

/// Default rolling window — matches Anthropic's 5-hour subscription cadence.
pub const DEFAULT_WINDOW: Duration = Duration::hours(5);

/// Default short-term burn-rate window.
pub const DEFAULT_BURN_WINDOW: Duration = Duration::minutes(30);

/// Minimum events in the burn window required before we report a rate.
/// Below this we'd be amplifying noise from one or two sparse calls.
pub const DEFAULT_MIN_BURN_EVENTS: usize = 3;

/// Per-model row in a [`WindowSummary::by_model`] breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByModel {
    /// Anthropic model name as it appears in the JSONL (e.g.
    /// `claude-sonnet-4-6`). May be the empty string for events that arrived
    /// without a model field.
    pub model: String,
    pub events: usize,
    pub total_tokens: u64,
}

/// Result of [`summarize_window`] — aggregated state for one rolling window
/// of `UsageEvent`s. Pure data, derivable from `(events, now, window,
/// burn_window, min_burn_events, window_anchor)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSummary {
    pub window_start: DateTime<Utc>,
    pub total_events_in_window: usize,
    pub total_tokens_in_window: u64,
    /// Tokens-per-minute averaged across the short burn window. `None` when
    /// fewer than `min_burn_events` events fall in the burn window — keeps
    /// the predictor away from "1 event, 5000 tokens/min, ship it" noise.
    pub recent_burn_tokens_per_min: Option<f64>,
    /// Per-model breakdown across the main window, sorted by total tokens
    /// descending (ties broken by model name ascending for determinism).
    pub by_model: Vec<ByModel>,
}

/// Summarize a slice of `UsageEvent`s over a rolling window ending at `now`.
///
/// `window` defines the main aggregation interval (events with
/// `ts >= now - window`). `burn_window` is the short interval used for the
/// burn-rate hint. Burn rate is `Some(tokens / burn_window_minutes)` only
/// when at least `min_burn_events` events fall in the burn window.
///
/// `window_anchor`: when `Some(reset)`, the main window is `[reset - window,
/// reset)` — pinned to Anthropic's server-reported reset timestamp so the cap
/// math is not skewed by local clock drift. `None` keeps the legacy
/// `now - window` window for callers without an OAuth reset (degraded path).
/// Burn math is always `now`-relative regardless of the anchor.
///
/// The function is pure: same input, same output. Defaults are exposed via
/// the `DEFAULT_*` consts for callers that want the canonical 5h / 30m / 3.
pub fn summarize_window(
    events: &[UsageEvent],
    now: DateTime<Utc>,
    window: Duration,
    burn_window: Duration,
    min_burn_events: usize,
    window_anchor: Option<DateTime<Utc>>,
) -> WindowSummary {
    // Anthropic's 5h cap window ENDS at the server-reported reset; the active
    // window is [reset - window, reset). Anchoring removes local clock-drift
    // error from the cap math (AGENTS.md v0.1.1). `None` keeps the legacy
    // now-relative window for callers without an OAuth reset (degraded path).
    let window_start = match window_anchor {
        Some(reset) => reset - window,
        None => now - window,
    };
    // When anchored, the cap window is the half-open interval
    // [reset - window, reset): events at or after the server reset belong to
    // the NEXT window, not this one. Unanchored (`None`) keeps the legacy
    // open-ended `[now - window, ..)` behavior so existing callers are
    // byte-identical.
    let window_end = window_anchor; // Some(reset) => exclusive upper bound
    let burn_window_start = now - burn_window;

    let mut by_model_map: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut total_events_in_window: usize = 0;
    let mut total_tokens_in_window: u64 = 0;
    let mut burn_tokens: u64 = 0;
    let mut burn_events: usize = 0;

    for ev in events {
        let in_main_window = ev.ts >= window_start && window_end.map_or(true, |end| ev.ts < end);
        if in_main_window {
            total_events_in_window += 1;
            let tokens = ev.total_tokens();
            total_tokens_in_window = total_tokens_in_window.saturating_add(tokens);
            let entry = by_model_map.entry(ev.model.clone()).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry.1.saturating_add(tokens);
        }
        if ev.ts >= burn_window_start {
            burn_events += 1;
            burn_tokens = burn_tokens.saturating_add(ev.total_tokens());
        }
    }

    let recent_burn_tokens_per_min = if burn_events >= min_burn_events {
        let minutes = burn_window.num_minutes().max(1) as f64;
        Some(burn_tokens as f64 / minutes)
    } else {
        None
    };

    // BTreeMap iter is name-ascending; stable_sort by total_tokens descending
    // preserves the name-ascending order for ties.
    let mut by_model: Vec<ByModel> = by_model_map
        .into_iter()
        .map(|(model, (events, total_tokens))| ByModel {
            model,
            events,
            total_tokens,
        })
        .collect();
    by_model.sort_by_key(|b| std::cmp::Reverse(b.total_tokens));

    WindowSummary {
        window_start,
        total_events_in_window,
        total_tokens_in_window,
        recent_burn_tokens_per_min,
        by_model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use claude_parser::{AccountType, DataSource, Provider};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap()
    }

    fn ev(ts: DateTime<Utc>, model: &str, input: u64, output: u64) -> UsageEvent {
        UsageEvent {
            ts,
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: None,
            request_id: None,
        }
    }

    #[test]
    fn empty_events_returns_zero_counts_and_no_burn() {
        let s = summarize_window(
            &[],
            now(),
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.total_events_in_window, 0);
        assert_eq!(s.total_tokens_in_window, 0);
        assert!(s.by_model.is_empty());
        assert_eq!(s.recent_burn_tokens_per_min, None);
        assert_eq!(s.window_start, now() - DEFAULT_WINDOW);
    }

    #[test]
    fn events_outside_window_are_ignored() {
        let n = now();
        let stale = ev(n - Duration::hours(6), "sonnet", 100, 50);
        let s = summarize_window(
            &[stale],
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.total_events_in_window, 0);
        assert_eq!(s.total_tokens_in_window, 0);
    }

    #[test]
    fn event_exactly_at_window_start_is_included() {
        let n = now();
        let edge = ev(n - DEFAULT_WINDOW, "sonnet", 100, 50);
        let s = summarize_window(
            &[edge],
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.total_events_in_window, 1);
        assert_eq!(s.total_tokens_in_window, 150);
    }

    #[test]
    fn single_in_window_event_is_counted_with_total_tokens() {
        let n = now();
        let e = UsageEvent {
            ts: n - Duration::minutes(10),
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 7,
            cache_read_input_tokens: 13,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: None,
            request_id: None,
        };
        let s = summarize_window(
            &[e],
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.total_events_in_window, 1);
        assert_eq!(s.total_tokens_in_window, 170); // 100 + 50 + 7 + 13
        assert_eq!(s.by_model.len(), 1);
        assert_eq!(s.by_model[0].model, "claude-sonnet-4-6");
        assert_eq!(s.by_model[0].events, 1);
        assert_eq!(s.by_model[0].total_tokens, 170);
    }

    #[test]
    fn burn_rate_none_when_fewer_than_min_events() {
        let n = now();
        let events = vec![
            ev(n - Duration::minutes(5), "sonnet", 1000, 500),
            ev(n - Duration::minutes(10), "sonnet", 1000, 500),
        ];
        let s = summarize_window(
            &events,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.recent_burn_tokens_per_min, None);
    }

    #[test]
    fn burn_rate_some_at_threshold() {
        let n = now();
        // 3 events in the 30-min burn window, 1500 tokens each = 4500 total
        // 4500 tokens / 30 minutes = 150 tokens/min.
        let events = vec![
            ev(n - Duration::minutes(5), "sonnet", 1000, 500),
            ev(n - Duration::minutes(10), "sonnet", 1000, 500),
            ev(n - Duration::minutes(20), "sonnet", 1000, 500),
        ];
        let s = summarize_window(
            &events,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.recent_burn_tokens_per_min, Some(150.0));
    }

    #[test]
    fn burn_rate_excludes_events_outside_burn_window() {
        let n = now();
        // Two in-burn-window events + one just outside the burn window
        // (35 min ago). Only 2 in burn -> below threshold -> None.
        let events = vec![
            ev(n - Duration::minutes(5), "sonnet", 1000, 500),
            ev(n - Duration::minutes(10), "sonnet", 1000, 500),
            ev(n - Duration::minutes(35), "sonnet", 1000, 500),
        ];
        let s = summarize_window(
            &events,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        // All 3 are inside the 5h main window:
        assert_eq!(s.total_events_in_window, 3);
        // But only 2 are inside the 30m burn window:
        assert_eq!(s.recent_burn_tokens_per_min, None);
    }

    #[test]
    fn by_model_sorted_descending_by_total_tokens() {
        let n = now();
        let events = vec![
            ev(n - Duration::minutes(5), "haiku", 100, 50), // 150
            ev(n - Duration::minutes(10), "opus", 1000, 500), // 1500
            ev(n - Duration::minutes(15), "sonnet", 400, 200), // 600
        ];
        let s = summarize_window(
            &events,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        let models: Vec<&str> = s.by_model.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(models, vec!["opus", "sonnet", "haiku"]);
    }

    #[test]
    fn tied_models_ordered_alphabetically_for_determinism() {
        let n = now();
        let events = vec![
            ev(n - Duration::minutes(5), "zebra", 100, 0),
            ev(n - Duration::minutes(5), "alpha", 100, 0),
            ev(n - Duration::minutes(5), "middle", 100, 0),
        ];
        let s = summarize_window(
            &events,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        // All tied at 100 tokens; expect alpha-ascending order preserved by
        // stable sort.
        let models: Vec<&str> = s.by_model.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(models, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn same_model_multiple_events_aggregated() {
        let n = now();
        let events = vec![
            ev(n - Duration::minutes(5), "sonnet", 100, 50),
            ev(n - Duration::minutes(15), "sonnet", 200, 100),
            ev(n - Duration::minutes(25), "sonnet", 50, 25),
        ];
        let s = summarize_window(
            &events,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.by_model.len(), 1);
        assert_eq!(s.by_model[0].events, 3);
        assert_eq!(s.by_model[0].total_tokens, 525); // 150 + 300 + 75
    }

    #[test]
    fn anchored_window_uses_reset_minus_window_not_now() {
        let n = now(); // 2026-05-14 12:00:00
                       // Server says the 5h window resets 2h from now → the active anchored
                       // window is [reset - 5h, reset) = [n - 3h, n + 2h).
        let reset = n + Duration::hours(2);
        // One event 4h ago: INSIDE the legacy now-5h window [n - 5h, n) but
        // OUTSIDE the anchored window (which starts at n - 3h). Same input,
        // both code paths — the only difference is the anchor.
        let evs = [ev(n - Duration::hours(4), "sonnet", 100, 50)];

        let anchored = summarize_window(
            &evs,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            Some(reset),
        );
        assert_eq!(anchored.window_start, reset - DEFAULT_WINDOW);
        assert_eq!(
            anchored.total_events_in_window, 0,
            "anchored window [n-3h, ..) must EXCLUDE the n-4h event"
        );

        let now_based = summarize_window(
            &evs,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(now_based.window_start, n - DEFAULT_WINDOW);
        assert_eq!(
            now_based.total_events_in_window, 1,
            "legacy now-window [n-5h, ..) INCLUDES it — the two paths genuinely differ"
        );
    }

    #[test]
    fn none_anchor_is_identical_to_now_minus_window() {
        let n = now();
        let e = ev(n - Duration::hours(1), "sonnet", 10, 5);
        let s = summarize_window(
            &[e],
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            None,
        );
        assert_eq!(s.window_start, n - DEFAULT_WINDOW);
        assert_eq!(s.total_events_in_window, 1);
    }

    #[test]
    fn anchored_window_excludes_events_at_or_after_reset() {
        let n = now(); // 2026-05-14 12:00:00
        let reset = n + Duration::hours(2); // anchored window [n-3h, n+2h)
        let evs = [
            ev(reset - Duration::minutes(1), "sonnet", 10, 5), // inside [.., reset)
            ev(reset, "sonnet", 10, 5),                        // AT reset -> excluded
            ev(reset + Duration::hours(1), "sonnet", 10, 5),   // after reset -> excluded
        ];
        let s = summarize_window(
            &evs,
            n,
            DEFAULT_WINDOW,
            DEFAULT_BURN_WINDOW,
            DEFAULT_MIN_BURN_EVENTS,
            Some(reset),
        );
        assert_eq!(s.window_start, reset - DEFAULT_WINDOW);
        assert_eq!(
            s.total_events_in_window, 1,
            "only the pre-reset event is in the half-open [reset-window, reset)"
        );
    }
}
