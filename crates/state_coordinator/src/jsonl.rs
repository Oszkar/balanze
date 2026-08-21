//! The single JSONL → (window summary, API-rate cost) synthesis.
//!
//! This is the ONE place the JSONL pipeline math lives (AGENTS.md §4 #8): the
//! one-shot CLI path (`snapshot_composer::compose`) and the live path (the
//! coordinator merge, watcher-driven) both call [`summarize_jsonl`], so the two
//! cannot drift. In particular the rolling-window anchor - Anthropic's
//! server-reported 5-hour reset - is applied identically on both paths. The
//! watcher used to compute its own window with a hard-coded `None` anchor and
//! silently diverged from the CLI here; centralizing the math fixes that.

use chrono::{DateTime, Datelike as _, TimeZone as _, Utc};
use claude_cost::{Cost, PriceTable, compute_cost};
use claude_parser::UsageEvent;
use window::{DEFAULT_BURN_WINDOW, DEFAULT_MIN_BURN_EVENTS, DEFAULT_WINDOW, summarize_window};

use crate::snapshot::JsonlSnapshot;

/// Both JSONL-fed snapshot cells, derived from one deduped event slice.
///
/// `cost` is `Err` only when no price table was available (the bundled table
/// failed to load - shouldn't happen on a release build); the `window` is
/// always produced. Mirrors the independent-cells model in `Snapshot`: a
/// price-table failure must not suppress the window summary.
pub struct JsonlCells {
    pub jsonl: JsonlSnapshot,
    pub cost: Result<Cost, String>,
}

/// Synthesize the JSONL window summary + API-rate cost estimate from a deduped
/// event slice. Pure (no I/O).
///
/// `anchor` is the OAuth-reported 5-hour reset when available; `summarize_window`
/// transparently falls back to the now-relative window when it is `None` or a
/// non-future timestamp. `prices` is `None` only when the bundled LiteLLM table
/// failed to load, in which case `cost` carries the error and the caller routes
/// it to `anthropic_api_cost_error`.
pub fn summarize_jsonl(
    events: &[UsageEvent],
    now: DateTime<Utc>,
    files_scanned: usize,
    anchor: Option<DateTime<Utc>>,
    prices: Option<&PriceTable>,
) -> JsonlCells {
    let window = summarize_window(
        events,
        now,
        DEFAULT_WINDOW,
        DEFAULT_BURN_WINDOW,
        DEFAULT_MIN_BURN_EVENTS,
        anchor,
    );
    let month_start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .expect("valid UTC month start");
    let current_month_events = events
        .iter()
        .filter(|event| event.ts >= month_start && event.ts <= now)
        .cloned()
        .collect::<Vec<_>>();
    let cost = match prices {
        Some(p) => Ok(compute_cost(&current_month_events, p)),
        None => Err("claude_cost: bundled price table unavailable".to_string()),
    };
    JsonlCells {
        jsonl: JsonlSnapshot {
            files_scanned,
            window,
        },
        cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn future_anchor_pins_window_start_to_reset_minus_window() {
        let reset = now() + chrono::Duration::minutes(90);
        let cells = summarize_jsonl(&[], now(), 0, Some(reset), None);
        assert_eq!(cells.jsonl.window.window_start, reset - DEFAULT_WINDOW);
    }

    #[test]
    fn no_anchor_uses_now_relative_window() {
        let cells = summarize_jsonl(&[], now(), 2, None, None);
        assert_eq!(cells.jsonl.window.window_start, now() - DEFAULT_WINDOW);
        assert_eq!(cells.jsonl.files_scanned, 2);
    }

    #[test]
    fn missing_price_table_yields_cost_error_but_still_a_window() {
        let cells = summarize_jsonl(&[], now(), 0, None, None);
        assert!(cells.cost.is_err(), "no price table => cost is Err");
        // The window is still produced regardless of the price-table failure.
        assert_eq!(cells.jsonl.window.total_events_in_window, 0);
    }

    #[test]
    fn api_rate_cost_is_bounded_to_the_current_calendar_month() {
        let mut events = crate::test_support::sample_events();
        let current = events[0].clone();
        let mut prior_month = current.clone();
        prior_month.ts = Utc.with_ymd_and_hms(2026, 4, 30, 23, 59, 0).unwrap();
        let mut future = current.clone();
        future.ts = now() + chrono::Duration::seconds(1);
        events = vec![prior_month, current.clone(), future];
        let prices = claude_cost::load_bundled_prices().unwrap();

        let cells = summarize_jsonl(&events, now(), 1, None, Some(&prices));
        let expected = compute_cost(&[current], &prices);

        assert_eq!(
            cells.cost.unwrap().total_micro_usd,
            expected.total_micro_usd
        );
    }
}
