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
        let cache_creation = optional_f64(entry, "cache_creation_input_token_cost", key)?;
        let cache_read = optional_f64(entry, "cache_read_input_token_cost", key)?;

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
    let v = entry.get(field).and_then(|v| v.as_f64()).ok_or_else(|| {
        CostError::PricesMissing(format!("model {model} missing required field {field}"))
    })?;
    validate_price(v, field, model)?;
    Ok(v)
}

fn optional_f64(
    entry: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    model: &str,
) -> Result<Option<f64>, CostError> {
    match entry.get(field) {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            let f = v.as_f64().ok_or_else(|| {
                CostError::PricesMissing(format!(
                    "model {model} field {field} present but not numeric"
                ))
            })?;
            validate_price(f, field, model)?;
            Ok(Some(f))
        }
    }
}

/// Validate a price value: finite, non-negative, and below `$1/token`.
///
/// The `$1/token` upper bound is a sanity guard against typos and unit errors
/// (e.g. someone vendoring `3` instead of `3e-6` for $3/M tokens). No real
/// Anthropic price is anywhere near this magnitude — Opus output is
/// `$75/M = 7.5e-5 USD/token`, so the bound leaves ~4 orders of magnitude of
/// headroom for any plausibly-priced future model. A price of `1.0` USD per
/// single token would imply a million-dollar prompt for a million tokens; if
/// that ever becomes a real price, the bound can be raised, but the more
/// likely cause is a corrupted refresh and we want a loud failure.
fn validate_price(usd_per_token: f64, field: &str, model: &str) -> Result<(), CostError> {
    if !usd_per_token.is_finite() {
        return Err(CostError::PricesMissing(format!(
            "model {model} field {field} is non-finite: {usd_per_token}"
        )));
    }
    if usd_per_token < 0.0 {
        return Err(CostError::PricesMissing(format!(
            "model {model} field {field} is negative: {usd_per_token}"
        )));
    }
    if usd_per_token >= 1.0 {
        return Err(CostError::PricesMissing(format!(
            "model {model} field {field} is implausibly large (>= $1/token): {usd_per_token}"
        )));
    }
    Ok(())
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

    #[test]
    fn parse_prices_rejects_negative_price() {
        let json = r#"{
            "claude-evil": {
                "input_cost_per_token": -1.0e-6,
                "output_cost_per_token": 1.5e-5
            }
        }"#;
        let err = parse_prices(json).unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("negative"), "got: {msg}");
                assert!(msg.contains("claude-evil"), "got: {msg}");
                assert!(msg.contains("input_cost_per_token"), "got: {msg}");
            }
        }
    }

    #[test]
    fn parse_prices_rejects_implausibly_large_price() {
        // 1.5 USD/token would mean $1.5M for a 1M-token conversation.
        // The validation bound is `>= 1.0` USD/token (strict — matches the
        // "below $1/token" doc + the bundled_prices_pass_sanity_scan check
        // which asserts nano values `< 1_000_000_000`).
        let json = r#"{
            "claude-typo": {
                "input_cost_per_token": 1.5,
                "output_cost_per_token": 1.5e-5
            }
        }"#;
        let err = parse_prices(json).unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("implausibly large"), "got: {msg}");
                assert!(msg.contains("$1/token"), "got: {msg}");
                assert!(msg.contains("claude-typo"), "got: {msg}");
            }
        }
    }

    #[test]
    fn parse_prices_rejects_price_at_exactly_one_dollar() {
        // Boundary: validate_price uses `>= 1.0` (strict), matching the doc's
        // "below $1/token" and the sanity scan's `< 1_000_000_000`.
        let json = r#"{
            "claude-boundary": {
                "input_cost_per_token": 1.0,
                "output_cost_per_token": 1.5e-5
            }
        }"#;
        let err = parse_prices(json).unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("implausibly large"), "got: {msg}");
            }
        }
    }

    #[test]
    fn parse_prices_rejects_negative_optional_cache_field() {
        let json = r#"{
            "claude-cache-bug": {
                "input_cost_per_token": 1.0e-6,
                "output_cost_per_token": 5.0e-6,
                "cache_read_input_token_cost": -1.0e-7
            }
        }"#;
        let err = parse_prices(json).unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("negative"), "got: {msg}");
                assert!(msg.contains("cache_read_input_token_cost"), "got: {msg}");
            }
        }
    }

    #[test]
    fn parse_prices_rejects_non_numeric_optional_field() {
        let json = r#"{
            "claude-typed-bad": {
                "input_cost_per_token": 1.0e-6,
                "output_cost_per_token": 5.0e-6,
                "cache_creation_input_token_cost": "not-a-number"
            }
        }"#;
        let err = parse_prices(json).unwrap_err();
        match err {
            CostError::PricesMissing(msg) => {
                assert!(msg.contains("not numeric"), "got: {msg}");
                assert!(
                    msg.contains("cache_creation_input_token_cost"),
                    "got: {msg}"
                );
            }
        }
    }

    #[test]
    fn provenance_const_matches_bundled_json_meta_commit_prefix() {
        // The build script extracts the commit prefix from the filename; the
        // data file's `_meta.commit` block holds the full SHA. If these drift
        // (e.g. a refresher updates one but not the other), this assertion
        // catches it. Build-time const must be a prefix of the JSON metadata.
        let raw: serde_json::Value = serde_json::from_str(BUNDLED_PRICES_JSON)
            .expect("bundled JSON parses for provenance check");
        let meta_commit = raw
            .get("_meta")
            .and_then(|m| m.get("commit"))
            .and_then(|c| c.as_str())
            .expect("bundled JSON has _meta.commit");
        assert!(
            meta_commit.starts_with(crate::PRICE_TABLE_COMMIT),
            "PRICE_TABLE_COMMIT ({}) is not a prefix of bundled JSON _meta.commit ({}). \
             Refresher likely updated the filename without updating the data \
             file, or vice versa.",
            crate::PRICE_TABLE_COMMIT,
            meta_commit,
        );

        let meta_date = raw
            .get("_meta")
            .and_then(|m| m.get("fetched_at"))
            .and_then(|c| c.as_str())
            .expect("bundled JSON has _meta.fetched_at");
        assert_eq!(
            meta_date,
            crate::PRICE_TABLE_DATE,
            "PRICE_TABLE_DATE ({}) does not match bundled JSON _meta.fetched_at ({}).",
            crate::PRICE_TABLE_DATE,
            meta_date,
        );
    }

    #[test]
    fn bundled_prices_pass_sanity_scan() {
        // Defense-in-depth: every entry in the shipped table has plausible
        // values. Catches refresh-corruption where one model's price
        // accidentally ends up at zero or wildly off.
        let table = load_bundled_prices().unwrap();
        assert!(
            table.models.len() >= 15,
            "bundled table has fewer models than expected: {}",
            table.models.len()
        );
        for (name, prices) in &table.models {
            assert!(
                prices.input_nano_per_token > 0,
                "model {name}: input price is zero or negative"
            );
            assert!(
                prices.output_nano_per_token > 0,
                "model {name}: output price is zero or negative"
            );
            // Sanity bound: no per-token price > 1 nano dollar (= $1/token).
            // (validate_price enforces this at parse, but assert at the
            // post-conversion stage too in case the conversion grew bugs.)
            // 1 USD/token = 1e9 nano/token.
            assert!(
                prices.input_nano_per_token < 1_000_000_000,
                "model {name}: input price >= $1/token ({} nano)",
                prices.input_nano_per_token
            );
            assert!(
                prices.output_nano_per_token < 1_000_000_000,
                "model {name}: output price >= $1/token ({} nano)",
                prices.output_nano_per_token
            );
            if let Some(cc) = prices.cache_creation_nano_per_token {
                assert!(
                    (0..1_000_000_000).contains(&cc),
                    "model {name}: cache_creation out of range ({cc})"
                );
            }
            if let Some(cr) = prices.cache_read_nano_per_token {
                assert!(
                    (0..1_000_000_000).contains(&cr),
                    "model {name}: cache_read out of range ({cr})"
                );
            }
        }
    }
}
