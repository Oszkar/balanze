//! Vendored Anthropic price table.
//!
//! Sourced from BerriAI/litellm's `model_prices_and_context_window.json`,
//! filtered to bare `claude-*` model names. The JSON is embedded at
//! compile time via `include_str!`; the file path is hardcoded here and
//! must be updated alongside `data/litellm-prices-*.json` whenever the
//! table is refreshed (see crate-level README).
//!
//! Storage uses **nano-USD per token** (`1e-9` USD) rather than micro-USD
//! per token because Anthropic's cache_read prices (≈$0.30/M tokens =
//! 0.3 micro-USD/token) would lose precision rounding to integer
//! micro-USD. Nano resolution is lossless for every entry in the
//! current vendored table.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::errors::CostError;

/// Parsed price table: per-model costs plus provenance metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceTable {
    /// Per-model price entries, keyed by Anthropic model name (e.g.
    /// `claude-sonnet-4-6`). The map is sorted because `BTreeMap`
    /// preserves key order — relevant for deterministic iteration in
    /// downstream consumers.
    pub models: BTreeMap<String, ModelPrices>,
    /// Short LiteLLM commit hash the snapshot was vendored from. Set by
    /// [`load_bundled_prices`] from the build-script-emitted const
    /// `PRICE_TABLE_COMMIT`.
    pub commit: &'static str,
    /// Vendoring date as `YYYY-MM-DD`. Set by [`load_bundled_prices`]
    /// from `PRICE_TABLE_DATE`.
    pub fetched_at: &'static str,
}

/// Per-model price entry.
///
/// All values are integer nano-USD per token (1 nano = `1e-9` USD).
/// Conversion from LiteLLM's f64 USD/token values is lossless for every
/// entry in the current Anthropic subset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPrices {
    pub input_nano_per_token: i64,
    pub output_nano_per_token: i64,
    /// `None` for older Claude models that lack prompt-caching pricing.
    pub cache_creation_nano_per_token: Option<i64>,
    /// `None` for older Claude models that lack prompt-caching pricing.
    pub cache_read_nano_per_token: Option<i64>,
}

/// Embedded JSON snapshot. **Hardcoded path** — when refreshing the price
/// table, update both the data filename and this line. The build script
/// validates the filename pattern but cannot rewrite this `include_str!`
/// because `include_str!` requires a string literal at parse time.
const BUNDLED_PRICES_JSON: &str = include_str!("../data/litellm-prices-e58a561-20260514.json");

/// Load the compile-time-embedded vendored price table.
///
/// Returns `Err(CostError::PricesMissing)` only if the embedded JSON is
/// malformed or has zero model entries — both of which are compile-time
/// impossible for the data this repo ships. The error variant exists for
/// the parser helper, which is also exercised on synthetic input in
/// unit tests.
pub fn load_bundled_prices() -> Result<PriceTable, CostError> {
    let mut table = parse_prices(BUNDLED_PRICES_JSON)?;
    table.commit = crate::PRICE_TABLE_COMMIT;
    table.fetched_at = crate::PRICE_TABLE_DATE;
    Ok(table)
}

/// Parse a JSON snapshot in the LiteLLM-subset format the crate ships.
///
/// Exposed within the crate so unit tests can exercise the parser on
/// synthetic input; downstream callers should use
/// [`load_bundled_prices`] instead.
pub(crate) fn parse_prices(json: &str) -> Result<PriceTable, CostError> {
    let raw: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CostError::PricesMissing(format!("JSON parse failed: {e}")))?;

    let obj = raw
        .as_object()
        .ok_or_else(|| CostError::PricesMissing("top-level JSON must be an object".to_string()))?;

    let mut models = BTreeMap::new();
    for (key, value) in obj {
        if key == "_meta" {
            continue;
        }

        let entry = value.as_object().ok_or_else(|| {
            CostError::PricesMissing(format!("entry for {key} is not a JSON object"))
        })?;

        let input = required_f64(entry, "input_cost_per_token", key)?;
        let output = required_f64(entry, "output_cost_per_token", key)?;
        let cache_creation = optional_f64(entry, "cache_creation_input_token_cost")?;
        let cache_read = optional_f64(entry, "cache_read_input_token_cost")?;

        models.insert(
            key.clone(),
            ModelPrices {
                input_nano_per_token: usd_per_token_to_nano(input),
                output_nano_per_token: usd_per_token_to_nano(output),
                cache_creation_nano_per_token: cache_creation.map(usd_per_token_to_nano),
                cache_read_nano_per_token: cache_read.map(usd_per_token_to_nano),
            },
        );
    }

    if models.is_empty() {
        return Err(CostError::PricesMissing(
            "price table contains zero model entries".to_string(),
        ));
    }

    Ok(PriceTable {
        models,
        // load_bundled_prices overwrites these; tests that use parse_prices
        // directly observe empty strings, which is intentional — provenance
        // is bound to the bundled file, not to arbitrary input.
        commit: "",
        fetched_at: "",
    })
}

fn required_f64(
    entry: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    model: &str,
) -> Result<f64, CostError> {
    entry.get(field).and_then(|v| v.as_f64()).ok_or_else(|| {
        CostError::PricesMissing(format!("model {model} missing required field {field}"))
    })
}

fn optional_f64(
    entry: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<f64>, CostError> {
    match entry.get(field) {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| {
            CostError::PricesMissing(format!("field {field} present but not numeric"))
        }),
    }
}

fn usd_per_token_to_nano(usd_per_token: f64) -> i64 {
    (usd_per_token * 1e9_f64).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_bundled_prices_returns_known_models() {
        let table = load_bundled_prices().expect("bundled prices must parse");
        assert!(!table.models.is_empty());
        assert!(table.models.contains_key("claude-sonnet-4-6"));
        assert!(table.models.contains_key("claude-opus-4-7"));
        assert!(table.models.contains_key("claude-haiku-4-5"));
    }

    #[test]
    fn load_bundled_prices_has_expected_values_for_sonnet_4_6() {
        let table = load_bundled_prices().unwrap();
        let prices = table.models.get("claude-sonnet-4-6").unwrap();
        assert_eq!(prices.input_nano_per_token, 3000);
        assert_eq!(prices.output_nano_per_token, 15000);
        assert_eq!(prices.cache_creation_nano_per_token, Some(3750));
        assert_eq!(prices.cache_read_nano_per_token, Some(300));
    }

    #[test]
    fn load_bundled_prices_provenance_consts_populated() {
        let table = load_bundled_prices().unwrap();
        assert!(!table.commit.is_empty());
        assert_eq!(table.commit, crate::PRICE_TABLE_COMMIT);
        assert_eq!(table.fetched_at, crate::PRICE_TABLE_DATE);
    }

    #[test]
    fn price_table_date_parses_as_naive_date() {
        use chrono::NaiveDate;
        let parsed = NaiveDate::parse_from_str(crate::PRICE_TABLE_DATE, "%Y-%m-%d")
            .expect("PRICE_TABLE_DATE must be parseable as YYYY-MM-DD");
        assert!(parsed > NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
    }

    #[test]
    fn price_table_commit_is_non_empty() {
        assert!(!crate::PRICE_TABLE_COMMIT.is_empty());
        assert!(
            crate::PRICE_TABLE_COMMIT.len() >= 7,
            "commit hash must be at least 7 chars, got {:?}",
            crate::PRICE_TABLE_COMMIT
        );
    }

    #[test]
    fn parse_prices_rejects_malformed_json() {
        let err = parse_prices("{not valid json").unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("JSON parse failed"), "got: {msg}")
            }
        }
    }

    #[test]
    fn parse_prices_rejects_non_object_top_level() {
        let err = parse_prices("[]").unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("must be an object"), "got: {msg}")
            }
        }
    }

    #[test]
    fn parse_prices_rejects_missing_required_field() {
        let json = r#"{
            "claude-test": {
                "output_cost_per_token": 1.0E-05
            }
        }"#;
        let err = parse_prices(json).unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("input_cost_per_token"), "got: {msg}");
                assert!(msg.contains("claude-test"), "got: {msg}");
            }
        }
    }

    #[test]
    fn parse_prices_skips_meta_key() {
        let json = r#"{
            "_meta": { "source": "test" },
            "claude-test": {
                "input_cost_per_token": 3.0E-06,
                "output_cost_per_token": 1.5E-05
            }
        }"#;
        let table = parse_prices(json).expect("should parse");
        assert_eq!(table.models.len(), 1);
        assert!(table.models.contains_key("claude-test"));
    }

    #[test]
    fn parse_prices_handles_optional_cache_fields_when_absent() {
        let json = r#"{
            "claude-no-cache": {
                "input_cost_per_token": 1.0E-06,
                "output_cost_per_token": 5.0E-06
            }
        }"#;
        let table = parse_prices(json).unwrap();
        let prices = table.models.get("claude-no-cache").unwrap();
        assert_eq!(prices.input_nano_per_token, 1000);
        assert_eq!(prices.output_nano_per_token, 5000);
        assert_eq!(prices.cache_creation_nano_per_token, None);
        assert_eq!(prices.cache_read_nano_per_token, None);
    }

    #[test]
    fn parse_prices_rejects_empty_models() {
        let json = r#"{ "_meta": {} }"#;
        let err = parse_prices(json).unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("zero model entries"), "got: {msg}")
            }
        }
    }
}
