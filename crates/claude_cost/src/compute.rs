//! Pure-function cost aggregation.
//!
//! Consumes `&[claude_parser::UsageEvent]` plus a `&PriceTable`, returns
//! a [`Cost`] with per-model breakdown, total, and a list of skipped
//! unknown models. The function is deterministic: same input, same
//! output, byte-identical when serialized.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use claude_parser::UsageEvent;

use crate::prices::PriceTable;

/// Aggregated cost result.
///
/// Sort discipline:
/// - `per_model` is sorted by `total_micro_usd` descending; ties are
///   broken by model name ascending so the order is deterministic for
///   serialization + snapshot dedup downstream.
/// - `skipped_models` is sorted alphabetically and deduplicated.
///
/// Saturation caveat: `total_micro_usd` is computed via `saturating_add`
/// across `per_model.total_micro_usd`. When one or more rows saturate at
/// `i64::MAX`, `total_micro_usd` also caps at `i64::MAX` rather than
/// overflowing — so the conceptual "sum of saturated rows" can exceed
/// what `total_micro_usd` reports. Callers asserting
/// `per_model.iter().map(|m| m.total_micro_usd).sum::<i64>() == total_micro_usd`
/// will be surprised at saturation. The test
/// `multiple_saturated_models_total_caps_at_i64_max` documents this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    pub per_model: Vec<ModelCost>,
    pub total_micro_usd: i64,
    pub skipped_models: Vec<String>,
    /// Total events the function saw, regardless of whether their model was
    /// found in the price table, was an unknown model, or had an empty model
    /// string. Use this for "events processed" metrics. **Do NOT sum
    /// `per_model.iter().map(|m| m.event_count)`** for that purpose —
    /// `event_count` only counts events whose model was known at price-table
    /// lookup time.
    pub total_event_count: usize,
    /// Events whose `event.model` was an empty string. `claude_parser` emits
    /// an empty model string when the JSONL line omits the model field —
    /// this is a parser-quirk count, not a price-table-gap count, so it
    /// lives separate from `skipped_models`.
    pub unparsed_event_count: usize,
}

/// Per-model cost row.
///
/// `event_count` counts only events whose model was found in the price
/// table for this model name. Unknown-model events route into
/// `Cost::skipped_models`; empty-model events route into
/// `Cost::unparsed_event_count`. For "total events processed" use
/// `Cost::total_event_count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCost {
    pub model: String,
    pub event_count: usize,
    pub input_micro_usd: i64,
    pub output_micro_usd: i64,
    pub cache_creation_micro_usd: i64,
    pub cache_read_micro_usd: i64,
    pub total_micro_usd: i64,
}

impl ModelCost {
    fn new(model: String) -> Self {
        Self {
            model,
            event_count: 0,
            input_micro_usd: 0,
            output_micro_usd: 0,
            cache_creation_micro_usd: 0,
            cache_read_micro_usd: 0,
            total_micro_usd: 0,
        }
    }
}

/// Compute cost from event slice + price table.
///
/// Pure function: no I/O, no logging above debug, no async. Same input
/// yields the same output (verified by the `deterministic_output` test).
///
/// Infallible by design: unknown models route into
/// [`Cost::skipped_models`] rather than raising an error. This is the
/// partial-success contract decided in /plan-eng-review: a brand-new
/// Claude model the user has access to but our vendored price table
/// doesn't know about should not brick the snapshot for every other
/// model in the same run.
pub fn compute_cost(events: &[UsageEvent], prices: &PriceTable) -> Cost {
    let mut by_model: BTreeMap<String, ModelCost> = BTreeMap::new();
    let mut skipped: BTreeMap<String, ()> = BTreeMap::new();
    let mut unparsed_event_count: usize = 0;
    let total_event_count = events.len();

    for event in events {
        let model = &event.model;

        // Empty model string: `claude_parser` emits this when the JSONL line
        // doesn't include a model field. Route to `unparsed_event_count`
        // rather than `skipped_models` so the empty string doesn't end up
        // as a blank entry in the renderer's "unknown models" list.
        if model.is_empty() {
            unparsed_event_count += 1;
            continue;
        }

        let Some(model_prices) = prices.models.get(model) else {
            skipped.insert(model.clone(), ());
            continue;
        };

        let entry = by_model
            .entry(model.clone())
            .or_insert_with(|| ModelCost::new(model.clone()));

        entry.event_count += 1;

        let input_micro =
            component_micro(event.input_tokens, Some(model_prices.input_nano_per_token));
        let output_micro = component_micro(
            event.output_tokens,
            Some(model_prices.output_nano_per_token),
        );
        let cache_creation_micro = component_micro(
            event.cache_creation_input_tokens,
            model_prices.cache_creation_nano_per_token,
        );
        let cache_read_micro = component_micro(
            event.cache_read_input_tokens,
            model_prices.cache_read_nano_per_token,
        );

        entry.input_micro_usd = entry.input_micro_usd.saturating_add(input_micro);
        entry.output_micro_usd = entry.output_micro_usd.saturating_add(output_micro);
        entry.cache_creation_micro_usd = entry
            .cache_creation_micro_usd
            .saturating_add(cache_creation_micro);
        entry.cache_read_micro_usd = entry.cache_read_micro_usd.saturating_add(cache_read_micro);
        entry.total_micro_usd = entry
            .total_micro_usd
            .saturating_add(input_micro)
            .saturating_add(output_micro)
            .saturating_add(cache_creation_micro)
            .saturating_add(cache_read_micro);
    }

    let mut per_model: Vec<ModelCost> = by_model.into_values().collect();
    per_model.sort_by(|a, b| {
        b.total_micro_usd
            .cmp(&a.total_micro_usd)
            .then_with(|| a.model.cmp(&b.model))
    });

    let total_micro_usd = per_model
        .iter()
        .fold(0_i64, |acc, m| acc.saturating_add(m.total_micro_usd));

    let skipped_models: Vec<String> = skipped.into_keys().collect();

    Cost {
        per_model,
        total_micro_usd,
        skipped_models,
        total_event_count,
        unparsed_event_count,
    }
}

/// Multiply (tokens, optional nano-per-token) → i64 micro-USD with
/// `i128` intermediate to avoid overflow, saturating at `i64::MAX` /
/// `i64::MIN` on the final cast. Returns 0 when the per-token price is
/// `None` (e.g. older models that lack cache_read pricing).
fn component_micro(tokens: u64, nano_per_token: Option<i64>) -> i64 {
    let nano = nano_per_token.unwrap_or(0);
    let product: i128 = (tokens as i128).saturating_mul(nano as i128);
    let micro_i128 = product / 1000;
    if micro_i128 > i64::MAX as i128 {
        i64::MAX
    } else if micro_i128 < i64::MIN as i128 {
        i64::MIN
    } else {
        micro_i128 as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use claude_parser::{AccountType, DataSource, Provider, UsageEvent};

    use crate::prices::ModelPrices;

    fn event(
        model: &str,
        input: u64,
        output: u64,
        cache_creation: u64,
        cache_read: u64,
    ) -> UsageEvent {
        UsageEvent {
            ts: Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap(),
            provider: Provider::Claude,
            account_type: AccountType::Api,
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: None,
            request_id: None,
        }
    }

    fn fixture_prices() -> PriceTable {
        let mut models = BTreeMap::new();
        // Same shape as the real claude-sonnet-4-6 entry.
        models.insert(
            "claude-sonnet-4-6".to_string(),
            ModelPrices {
                input_nano_per_token: 3000,                // $3.00 / M
                output_nano_per_token: 15000,              // $15.00 / M
                cache_creation_nano_per_token: Some(3750), // $3.75 / M
                cache_read_nano_per_token: Some(300),      // $0.30 / M
            },
        );
        // Older-style entry without caching.
        models.insert(
            "claude-3-haiku-no-cache".to_string(),
            ModelPrices {
                input_nano_per_token: 250,
                output_nano_per_token: 1250,
                cache_creation_nano_per_token: None,
                cache_read_nano_per_token: None,
            },
        );
        // Cheaper-than-sonnet entry for sort tests.
        models.insert(
            "claude-cheap".to_string(),
            ModelPrices {
                input_nano_per_token: 1000,
                output_nano_per_token: 5000,
                cache_creation_nano_per_token: None,
                cache_read_nano_per_token: None,
            },
        );
        PriceTable {
            models,
            commit: "test",
            fetched_at: "2026-05-14",
        }
    }

    #[test]
    fn empty_events_produces_zero_cost() {
        let cost = compute_cost(&[], &fixture_prices());
        assert_eq!(cost.total_micro_usd, 0);
        assert!(cost.per_model.is_empty());
        assert!(cost.skipped_models.is_empty());
        assert_eq!(cost.total_event_count, 0);
        assert_eq!(cost.unparsed_event_count, 0);
    }

    #[test]
    fn single_event_known_model_computes_correctly() {
        // 1M input + 1M output @ sonnet-4-6:
        //   input:  1e6 * 3000 nano = 3e9 nano = 3e6 micro = $3.00
        //   output: 1e6 * 15000 nano = 1.5e10 nano = 1.5e7 micro = $15.00
        //   total:  1.8e7 micro = $18.00
        let cost = compute_cost(
            &[event("claude-sonnet-4-6", 1_000_000, 1_000_000, 0, 0)],
            &fixture_prices(),
        );
        assert_eq!(cost.per_model.len(), 1);
        let m = &cost.per_model[0];
        assert_eq!(m.model, "claude-sonnet-4-6");
        assert_eq!(m.event_count, 1);
        assert_eq!(m.input_micro_usd, 3_000_000);
        assert_eq!(m.output_micro_usd, 15_000_000);
        assert_eq!(m.cache_creation_micro_usd, 0);
        assert_eq!(m.cache_read_micro_usd, 0);
        assert_eq!(m.total_micro_usd, 18_000_000);
        assert_eq!(cost.total_micro_usd, 18_000_000);
    }

    #[test]
    fn multiple_events_same_model_aggregate() {
        let events = vec![
            event("claude-sonnet-4-6", 1000, 100, 0, 0),
            event("claude-sonnet-4-6", 2000, 200, 0, 0),
            event("claude-sonnet-4-6", 3000, 300, 0, 0),
        ];
        let cost = compute_cost(&events, &fixture_prices());
        assert_eq!(cost.per_model.len(), 1);
        let m = &cost.per_model[0];
        assert_eq!(m.event_count, 3);
        // input:  6000 * 3000 nano = 18e6 nano = 18000 micro
        // output: 600  * 15000 nano = 9e6 nano = 9000 micro
        // total:  27000 micro
        assert_eq!(m.input_micro_usd, 18_000);
        assert_eq!(m.output_micro_usd, 9_000);
        assert_eq!(m.total_micro_usd, 27_000);
    }

    #[test]
    fn multiple_models_appear_in_separate_rows_sorted_desc() {
        let events = vec![
            // 1M tokens × per-token prices (in nano), /1000 → micro:
            event("claude-cheap", 1_000_000, 0, 0, 0), // 1_000_000 micro
            event("claude-sonnet-4-6", 1_000_000, 0, 0, 0), // 3_000_000 micro
            event("claude-3-haiku-no-cache", 1_000_000, 0, 0, 0), //   250_000 micro
        ];
        let cost = compute_cost(&events, &fixture_prices());
        assert_eq!(cost.per_model.len(), 3);
        assert_eq!(cost.per_model[0].model, "claude-sonnet-4-6");
        assert_eq!(cost.per_model[1].model, "claude-cheap");
        assert_eq!(cost.per_model[2].model, "claude-3-haiku-no-cache");
    }

    #[test]
    fn tied_totals_sort_by_model_name_asc() {
        let mut models = BTreeMap::new();
        let row = ModelPrices {
            input_nano_per_token: 1000,
            output_nano_per_token: 0,
            cache_creation_nano_per_token: None,
            cache_read_nano_per_token: None,
        };
        models.insert("claude-alpha".to_string(), row.clone());
        models.insert("claude-beta".to_string(), row);
        let prices = PriceTable {
            models,
            commit: "t",
            fetched_at: "t",
        };
        let events = vec![
            event("claude-beta", 1_000_000, 0, 0, 0),
            event("claude-alpha", 1_000_000, 0, 0, 0),
        ];
        let cost = compute_cost(&events, &prices);
        assert_eq!(cost.per_model.len(), 2);
        assert_eq!(cost.per_model[0].model, "claude-alpha");
        assert_eq!(cost.per_model[1].model, "claude-beta");
        assert_eq!(
            cost.per_model[0].total_micro_usd,
            cost.per_model[1].total_micro_usd
        );
    }

    #[test]
    fn zero_token_event_contributes_zero_not_skipped() {
        let cost = compute_cost(&[event("claude-sonnet-4-6", 0, 0, 0, 0)], &fixture_prices());
        assert_eq!(cost.per_model.len(), 1);
        let m = &cost.per_model[0];
        assert_eq!(m.event_count, 1);
        assert_eq!(m.total_micro_usd, 0);
        assert!(cost.skipped_models.is_empty());
    }

    #[test]
    fn unknown_model_added_to_skipped() {
        let cost = compute_cost(
            &[event("claude-not-yet-shipped", 1000, 100, 0, 0)],
            &fixture_prices(),
        );
        assert!(cost.per_model.is_empty());
        assert_eq!(cost.total_micro_usd, 0);
        assert_eq!(
            cost.skipped_models,
            vec!["claude-not-yet-shipped".to_string()]
        );
    }

    #[test]
    fn same_unknown_model_deduped_in_skipped() {
        let events = vec![
            event("claude-not-yet-shipped", 1000, 100, 0, 0),
            event("claude-not-yet-shipped", 2000, 200, 0, 0),
            event("claude-also-unknown", 100, 10, 0, 0),
        ];
        let cost = compute_cost(&events, &fixture_prices());
        assert!(cost.per_model.is_empty());
        assert_eq!(
            cost.skipped_models,
            vec![
                "claude-also-unknown".to_string(),
                "claude-not-yet-shipped".to_string(),
            ]
        );
    }

    #[test]
    fn skipped_models_sorted_alphabetically() {
        let events = vec![
            event("claude-zeta", 100, 10, 0, 0),
            event("claude-alpha-unknown", 100, 10, 0, 0),
            event("claude-beta-unknown", 100, 10, 0, 0),
        ];
        let cost = compute_cost(&events, &fixture_prices());
        assert_eq!(
            cost.skipped_models,
            vec![
                "claude-alpha-unknown".to_string(),
                "claude-beta-unknown".to_string(),
                "claude-zeta".to_string(),
            ]
        );
    }

    #[test]
    fn cache_creation_with_none_price_contributes_zero() {
        let cost = compute_cost(
            &[event("claude-3-haiku-no-cache", 0, 0, 100_000, 0)],
            &fixture_prices(),
        );
        let m = &cost.per_model[0];
        assert_eq!(m.cache_creation_micro_usd, 0);
        assert_eq!(m.total_micro_usd, 0);
    }

    #[test]
    fn cache_read_uses_dedicated_price() {
        // 1M cache_read tokens @ 300 nano/token = 3e8 nano = 3e5 micro = $0.30
        let cost = compute_cost(
            &[event("claude-sonnet-4-6", 0, 0, 0, 1_000_000)],
            &fixture_prices(),
        );
        let m = &cost.per_model[0];
        assert_eq!(m.cache_read_micro_usd, 300_000);
        assert_eq!(m.input_micro_usd, 0);
        assert_eq!(m.output_micro_usd, 0);
        assert_eq!(m.cache_creation_micro_usd, 0);
        assert_eq!(m.total_micro_usd, 300_000);
    }

    #[test]
    fn huge_token_count_saturates_without_panic() {
        // u64::MAX / 4 = ~4.6e18 input tokens × 3000 nano/token = ~1.4e22 nano
        // = ~1.4e19 micro; i64::MAX is ~9.2e18 — saturation expected.
        let cost = compute_cost(
            &[event("claude-sonnet-4-6", u64::MAX / 4, 0, 0, 0)],
            &fixture_prices(),
        );
        assert_eq!(cost.per_model.len(), 1);
        assert_eq!(cost.per_model[0].input_micro_usd, i64::MAX);
        assert_eq!(cost.per_model[0].total_micro_usd, i64::MAX);
    }

    #[test]
    fn deterministic_output() {
        let prices = fixture_prices();
        let events = vec![
            event("claude-sonnet-4-6", 1234, 567, 89, 12),
            event("claude-cheap", 999, 111, 0, 0),
            event("claude-unknown-x", 50, 5, 0, 0),
            event("claude-unknown-a", 5, 1, 0, 0),
        ];
        let a = compute_cost(&events, &prices);
        let b = compute_cost(&events, &prices);
        assert_eq!(a, b);
        let a_json = serde_json::to_string(&a).unwrap();
        let b_json = serde_json::to_string(&b).unwrap();
        assert_eq!(a_json, b_json);
    }

    #[test]
    fn subscription_account_type_still_computes() {
        let mut ev = event("claude-sonnet-4-6", 1000, 100, 0, 0);
        ev.account_type = AccountType::Subscription;
        let cost = compute_cost(&[ev], &fixture_prices());
        assert_eq!(cost.per_model.len(), 1);
        assert!(cost.total_micro_usd > 0);
    }

    #[test]
    fn integration_smoke_mixed_events() {
        let prices = fixture_prices();
        let events = vec![
            // Two sonnet events
            event("claude-sonnet-4-6", 1_000, 100, 0, 0),
            event("claude-sonnet-4-6", 2_000, 200, 0, 100),
            // One cheap event
            event("claude-cheap", 5_000, 500, 0, 0),
            // Two events on the same unknown model
            event("claude-future-model", 100, 10, 0, 0),
            event("claude-future-model", 200, 20, 0, 0),
        ];
        let cost = compute_cost(&events, &prices);

        assert_eq!(cost.per_model.len(), 2);
        assert_eq!(cost.skipped_models, vec!["claude-future-model".to_string()]);
        assert_eq!(cost.total_event_count, 5);
        assert_eq!(cost.unparsed_event_count, 0);
        // event_count only counts priced events (3); the 2 skipped ones are
        // accounted for via total_event_count.
        let priced_event_count: usize = cost.per_model.iter().map(|m| m.event_count).sum();
        assert_eq!(priced_event_count, 3);
        assert_eq!(
            cost.total_event_count - priced_event_count - cost.unparsed_event_count,
            2 // events with unknown model
        );

        // Sonnet totals:
        //   input: (1000+2000)*3000 nano = 9e6 nano = 9000 micro
        //   output: 300*15000 nano = 4.5e6 nano = 4500 micro
        //   cache_read: 100*300 nano = 3e4 nano = 30 micro
        //   total: 13530 micro
        let sonnet = cost
            .per_model
            .iter()
            .find(|m| m.model == "claude-sonnet-4-6")
            .unwrap();
        assert_eq!(sonnet.event_count, 2);
        assert_eq!(sonnet.input_micro_usd, 9_000);
        assert_eq!(sonnet.output_micro_usd, 4_500);
        assert_eq!(sonnet.cache_read_micro_usd, 30);
        assert_eq!(sonnet.total_micro_usd, 13_530);

        // Cheap totals:
        //   input: 5000*1000 nano = 5e6 nano = 5000 micro
        //   output: 500*5000 nano = 2.5e6 nano = 2500 micro
        //   total: 7500 micro
        let cheap = cost
            .per_model
            .iter()
            .find(|m| m.model == "claude-cheap")
            .unwrap();
        assert_eq!(cheap.event_count, 1);
        assert_eq!(cheap.input_micro_usd, 5_000);
        assert_eq!(cheap.output_micro_usd, 2_500);
        assert_eq!(cheap.total_micro_usd, 7_500);

        // Sort order check: sonnet > cheap.
        assert_eq!(cost.per_model[0].model, "claude-sonnet-4-6");
        assert_eq!(cost.per_model[1].model, "claude-cheap");

        // Grand total: 13530 + 7500 = 21030
        assert_eq!(cost.total_micro_usd, 21_030);
    }

    #[test]
    fn empty_model_event_counted_as_unparsed_not_skipped() {
        // claude_parser emits "" for `model` when the JSONL omits the field.
        // It must NOT show up in skipped_models — that list is for genuinely
        // unknown model NAMES; empty-string is a parser-quirk separate channel.
        let events = vec![event("", 1000, 100, 0, 0), event("", 2000, 200, 0, 0)];
        let cost = compute_cost(&events, &fixture_prices());
        assert!(cost.per_model.is_empty());
        assert!(
            cost.skipped_models.is_empty(),
            "empty-string model leaked into skipped_models: {:?}",
            cost.skipped_models
        );
        assert_eq!(cost.unparsed_event_count, 2);
        assert_eq!(cost.total_event_count, 2);
        assert_eq!(cost.total_micro_usd, 0);
    }

    #[test]
    fn total_event_count_counts_all_event_types() {
        // 1 priced + 1 skipped (unknown model) + 1 unparsed (empty model) =
        // total_event_count of 3. event_count for the priced model is 1.
        let events = vec![
            event("claude-sonnet-4-6", 1000, 100, 0, 0),
            event("claude-future-model", 1000, 100, 0, 0),
            event("", 1000, 100, 0, 0),
        ];
        let cost = compute_cost(&events, &fixture_prices());
        assert_eq!(cost.total_event_count, 3);
        assert_eq!(cost.unparsed_event_count, 1);
        assert_eq!(cost.skipped_models, vec!["claude-future-model".to_string()]);
        assert_eq!(cost.per_model.len(), 1);
        assert_eq!(cost.per_model[0].event_count, 1);
    }

    #[test]
    fn multiple_saturated_models_total_caps_at_i64_max() {
        // F4 from adversarial review: when 2+ models each saturate to i64::MAX,
        // the grand total also caps at i64::MAX rather than overflowing.
        // The conceptual sum (2 * i64::MAX) would exceed i64::MAX; the
        // saturating-add semantics protect against panic but mean callers
        // cannot assert `sum(rows) == total` at saturation. This test pins
        // the documented behavior.
        let mut models = BTreeMap::new();
        models.insert(
            "claude-huge-a".to_string(),
            ModelPrices {
                input_nano_per_token: 3000,
                output_nano_per_token: 0,
                cache_creation_nano_per_token: None,
                cache_read_nano_per_token: None,
            },
        );
        models.insert(
            "claude-huge-b".to_string(),
            ModelPrices {
                input_nano_per_token: 3000,
                output_nano_per_token: 0,
                cache_creation_nano_per_token: None,
                cache_read_nano_per_token: None,
            },
        );
        let prices = PriceTable {
            models,
            commit: "t",
            fetched_at: "t",
        };
        // u64::MAX/4 tokens × 3000 nano = ~1.4e22 nano = ~1.4e19 micro,
        // far exceeding i64::MAX (~9.2e18). Saturates.
        let events = vec![
            event("claude-huge-a", u64::MAX / 4, 0, 0, 0),
            event("claude-huge-b", u64::MAX / 4, 0, 0, 0),
        ];
        let cost = compute_cost(&events, &prices);
        assert_eq!(cost.per_model.len(), 2);
        assert_eq!(cost.per_model[0].total_micro_usd, i64::MAX);
        assert_eq!(cost.per_model[1].total_micro_usd, i64::MAX);
        // Grand total saturates at i64::MAX — NOT i64::MAX * 2.
        assert_eq!(cost.total_micro_usd, i64::MAX);
        // Documenting that the naive sum is meaningless at saturation:
        // sum(saturating) would overflow without saturating_add. We rely
        // on the iter().fold() in compute_cost to handle this.
    }
}
