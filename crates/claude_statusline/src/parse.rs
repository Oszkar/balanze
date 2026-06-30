use chrono::{TimeZone, Utc};
use serde::Deserialize;

use crate::errors::StatuslineError;
use crate::types::{RateLimits, RateWindow, StatuslineSnapshot};

#[derive(Debug, Deserialize)]
struct RawRoot {
    version: Option<String>,
    cost: Option<RawCost>,
    rate_limits: Option<RawRateLimits>,
    model: Option<RawModel>,
    context_window: Option<RawContextWindow>,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    total_cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    // Cosmetic/display-only: an absent, null, OR wrong-typed `display_name`
    // degrades to `None` (via `lenient_opt`) rather than failing the whole
    // parse. A drift here drops just the model segment; it must never blank the
    // statusline (including the valuable rate-limit data) over a display field.
    #[serde(default, deserialize_with = "lenient_opt")]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawContextWindow {
    // Cosmetic/display-only (integer in real payloads, e.g. 83; fractional
    // elsewhere, e.g. 4.2 - hence f32). Absent/null/wrong-typed degrades to
    // `None` via `lenient_opt`, dropping just the context segment.
    #[serde(default, deserialize_with = "lenient_opt")]
    used_percentage: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct RawRateLimits {
    // Block-level `null` is treated the same as an absent block (`None`), by
    // design: plain `Option` semantics, matching `cost` / `total_cost_usd`.
    // `null` is a common serializer encoding for "no window" (e.g. a plan
    // without a 7-day cap), so hard-erroring here would blank the whole
    // payload over a legitimate input. The absent-vs-null distinction (see
    // `deserialize_non_null`) applies only to required numeric fields INSIDE
    // a present window object, where `null` means a half-formed record.
    five_hour: Option<RawWindow>,
    seven_day: Option<RawWindow>,
}

#[derive(Debug, Deserialize)]
struct RawWindow {
    // `#[serde(default)]` + `deserialize_non_null` make an ABSENT field degrade
    // to `None` (forward-compat for a future field rename — see
    // `window_or_drop`), while a present `null` or wrong-TYPE value still errors
    // as `SchemaDrift`. Forward-compat covers renames, not corrupt/null values
    // for a required numeric field.
    #[serde(default, deserialize_with = "deserialize_non_null")]
    used_percentage: Option<f32>,
    /// Unix epoch SECONDS (per the documented schema).
    #[serde(default, deserialize_with = "deserialize_non_null")]
    resets_at: Option<i64>,
}

/// Deserialize a *present* field but treat an explicit `null` as an error, not
/// `None`. Paired with `#[serde(default)]` on the field: an ABSENT field → `None`
/// (this fn isn't called), a present `null` → the inner `T::deserialize` fails
/// (a required numeric can't be null) → `SchemaDrift`, a present value →
/// `Some(value)`. So the degrade-to-`None` forward-compat applies only to
/// genuinely-missing fields, never to null/corrupt values.
fn deserialize_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Deserialize an OPTIONAL COSMETIC field that must never fail the whole parse.
/// The opposite of `deserialize_non_null`: where required rate-limit numerics
/// surface a wrong-type/null as `SchemaDrift`, a display-only field (model name,
/// context %) degrades any wrong-typed or null value to `None` so a single drift
/// drops just that segment instead of blanking the entire statusline. Routes the
/// value through `serde_json::Value` and keeps it only if it converts to `T`.
fn lenient_opt<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

/// Parse the Claude Code statusLine stdin payload. Pure, infallible except
/// for invalid JSON or a present-but-wrong-shape required subfield. Unknown
/// fields are tolerated; absent optional blocks become `None`.
pub fn parse(input: &str) -> Result<StatuslineSnapshot, StatuslineError> {
    // Strip a single leading UTF-8 BOM (U+FEFF) before deserializing. serde_json
    // does not skip it, and some callers (e.g. a PowerShell pipe under a
    // UTF-8-with-BOM OutputEncoding) prepend one. Claude Code itself sends clean
    // UTF-8; this is defensive, consistent with the statusline drift tolerance.
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let raw: RawRoot = serde_json::from_str(input).map_err(|e| match e.classify() {
        serde_json::error::Category::Data => StatuslineError::SchemaDrift {
            message: e.to_string(),
        },
        _ => StatuslineError::InvalidJson(e.to_string()),
    })?;

    let session_cost_micro_usd = raw.cost.and_then(|c| c.total_cost_usd).map(usd_to_micro);

    let rate_limits = raw.rate_limits.map(|rl| RateLimits {
        five_hour: window_or_drop("five_hour", rl.five_hour),
        seven_day: window_or_drop("seven_day", rl.seven_day),
    });

    Ok(StatuslineSnapshot {
        rate_limits,
        session_cost_micro_usd,
        claude_code_version: raw.version,
        model_display_name: raw.model.and_then(|m| m.display_name),
        context_used_percent: raw.context_window.and_then(|c| c.used_percentage),
    })
}

/// Convert a raw window block to a `RateWindow`, or drop it (`None`).
///
/// A block that is *present but missing* a required field (e.g. a future
/// statusLine field rename) degrades to `None` — that one window is dropped
/// rather than failing the whole payload, so a single rename can't blank the
/// user's shell prompt or take out the other window + the session cost. The
/// same drop-with-warn applies to a `resets_at` outside chrono's representable
/// range, instead of rewriting it to the Unix epoch. A
/// present `null` or wrong-TYPE field still surfaces as `SchemaDrift` upstream
/// (it fails `serde` before we get here — see `deserialize_non_null`), so
/// genuine corruption is not silently swallowed.
///
/// A dropped window is logged at `warn!`. `balanze-cli` installs a `tracing`
/// subscriber for every subcommand (including `statusline`), so the warning is
/// observable on stderr when the env filter enables warn-level (e.g.
/// `RUST_LOG=warn`); it goes to stderr, never the statusLine's stdout, so it
/// can't pollute the prompt. With no subscriber installed (a library embedding
/// the parser) it is a silent no-op.
fn window_or_drop(name: &str, raw: Option<RawWindow>) -> Option<RateWindow> {
    let raw = raw?;
    match (raw.used_percentage, raw.resets_at) {
        (Some(used_percent), Some(secs)) => {
            // An out-of-range epoch (beyond chrono's ~±262,000-year span) is
            // corrupt wire data: drop the window visibly rather than rewrite
            // it to a plausible-looking 1970-01-01.
            let Some(resets_at) = Utc.timestamp_opt(secs, 0).single() else {
                tracing::warn!(
                    "claude_statusline: dropping `{name}` rate-limit window - \
                     out-of-range resets_at={secs}"
                );
                return None;
            };
            Some(RateWindow {
                used_percent,
                resets_at,
            })
        }
        _ => {
            tracing::warn!(
                "claude_statusline: dropping `{name}` rate-limit window — present but \
                 missing used_percentage/resets_at (statusLine schema drift?)"
            );
            None
        }
    }
}

/// Dollars (f64, from the wire) → i64 micro-USD, round half away from zero,
/// saturating. This is the ONLY f64→money conversion; everything downstream
/// is i64 micro-USD (AGENTS.md §2.1).
fn usd_to_micro(usd: f64) -> i64 {
    let scaled = usd * 1_000_000.0;
    let rounded = scaled.round();
    if rounded >= i64::MAX as f64 {
        i64::MAX
    } else if rounded <= i64::MIN as f64 {
        i64::MIN
    } else {
        rounded as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
      "version":"2.1.140","model":{"id":"claude-opus-4-7","display_name":"Opus"},
      "workspace":{"current_dir":"/x","project_dir":"/x"},
      "cost":{"total_cost_usd":12.5,"total_duration_ms":1000,"total_lines_added":3},
      "context_window":{"total_input_tokens":1,"used_percentage":4.2},
      "rate_limits":{
        "five_hour":{"used_percentage":13.0,"resets_at":1747650600},
        "seven_day":{"used_percentage":44.0,"resets_at":1747915200}
      }}"#;

    #[test]
    fn parses_full_pro_max_payload() {
        let s = parse(FULL).expect("parses");
        let rl = s.rate_limits.expect("rate_limits present");
        let fh = rl.five_hour.expect("five_hour");
        assert!((fh.used_percent - 13.0).abs() < 1e-4);
        assert_eq!(fh.resets_at.timestamp(), 1747650600);
        assert!((rl.seven_day.unwrap().used_percent - 44.0).abs() < 1e-4);
        assert_eq!(s.session_cost_micro_usd, Some(12_500_000));
        assert_eq!(s.claude_code_version.as_deref(), Some("2.1.140"));
    }

    #[test]
    fn tolerates_leading_utf8_bom() {
        // A caller (e.g. a PowerShell pipe with a UTF-8-with-BOM OutputEncoding)
        // can prepend a U+FEFF byte-order mark; serde_json does not skip it, so
        // a single leading BOM is stripped before deserializing. Claude Code
        // sends clean UTF-8 - this is defensive drift tolerance.
        let with_bom = format!("\u{feff}{FULL}");
        let s = parse(&with_bom).expect("parses despite a leading BOM");
        assert_eq!(s.session_cost_micro_usd, Some(12_500_000));
        assert!(s.rate_limits.is_some());
    }

    #[test]
    fn missing_rate_limits_is_none_not_error() {
        let body = r#"{"version":"2.1.140","cost":{"total_cost_usd":1.0}}"#;
        let s = parse(body).expect("parses without rate_limits");
        assert!(s.rate_limits.is_none());
        assert_eq!(s.session_cost_micro_usd, Some(1_000_000));
    }

    #[test]
    fn missing_cost_is_none() {
        let s = parse(r#"{"version":"2.1.140"}"#).expect("parses");
        assert!(s.session_cost_micro_usd.is_none());
        assert!(s.rate_limits.is_none());
    }

    #[test]
    fn invalid_json_is_invalid_json_error() {
        match parse("{not json") {
            Err(StatuslineError::InvalidJson(_)) => {}
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    #[test]
    fn wrong_type_required_subfield_is_schema_drift() {
        let body = r#"{"rate_limits":{"five_hour":{"used_percentage":1.0,"resets_at":"soon"}}}"#;
        match parse(body) {
            Err(StatuslineError::SchemaDrift { .. }) => {}
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn window_missing_field_degrades_to_none_not_schema_drift() {
        // A present window block missing `resets_at` (e.g. a future field
        // rename) drops just that window to None — the OTHER window AND the
        // session cost survive, rather than erroring the whole payload (which
        // would blank the shell prompt for one renamed sub-field).
        let body = r#"{
          "cost":{"total_cost_usd":2.5},
          "rate_limits":{
            "five_hour":{"used_percentage":13.0},
            "seven_day":{"used_percentage":44.0,"resets_at":1747915200}
          }}"#;
        let s = parse(body).expect("a partial window must NOT fail the whole payload");
        let rl = s.rate_limits.expect("rate_limits present");
        assert!(
            rl.five_hour.is_none(),
            "five_hour missing resets_at ⇒ dropped to None"
        );
        assert!(rl.seven_day.is_some(), "seven_day intact ⇒ preserved");
        assert_eq!(
            s.session_cost_micro_usd,
            Some(2_500_000),
            "session cost still parsed"
        );
    }

    #[test]
    fn explicit_null_required_field_is_schema_drift_not_dropped() {
        // An explicit `null` for a required numeric is corruption, not a missing
        // field — it must hard-error (SchemaDrift), NOT silently drop the window
        // (which `Option<T>` would do by mapping null → None). Distinguishes
        // absent (degrade) from present-null (error).
        let body = r#"{"rate_limits":{"five_hour":{"used_percentage":1.0,"resets_at":null}}}"#;
        match parse(body) {
            Err(StatuslineError::SchemaDrift { .. }) => {}
            other => panic!("expected SchemaDrift for explicit null, got {other:?}"),
        }
    }

    #[test]
    fn out_of_range_resets_at_drops_window_not_rewritten_to_epoch() {
        // i64::MAX seconds is far outside chrono's ~±262,000-year range.
        // The window must be dropped (None), NOT fabricated as 1970-01-01;
        // the other window and the session cost survive.
        let body = r#"{
          "cost":{"total_cost_usd":2.5},
          "rate_limits":{
            "five_hour":{"used_percentage":13.0,"resets_at":9223372036854775807},
            "seven_day":{"used_percentage":44.0,"resets_at":1747915200}
          }}"#;
        let s = parse(body).expect("an out-of-range resets_at must not fail the payload");
        let rl = s.rate_limits.expect("rate_limits present");
        assert!(
            rl.five_hour.is_none(),
            "out-of-range resets_at ⇒ window dropped, not epoch-rewritten"
        );
        assert!(rl.seven_day.is_some(), "seven_day intact ⇒ preserved");
        assert_eq!(s.session_cost_micro_usd, Some(2_500_000));
    }

    #[test]
    fn unknown_fields_tolerated_and_dollars_round_half_away() {
        let body = r#"{"brand_new_field":42,"cost":{"total_cost_usd":1.2345675}}"#;
        let s = parse(body).expect("tolerates unknown fields");
        assert_eq!(s.session_cost_micro_usd, Some(1_234_568));
    }

    #[test]
    fn one_window_present_other_absent() {
        let body =
            r#"{"rate_limits":{"five_hour":{"used_percentage":9.0,"resets_at":1747650600}}}"#;
        let rl = parse(body).unwrap().rate_limits.unwrap();
        assert!(rl.five_hour.is_some());
        assert!(rl.seven_day.is_none());
    }

    #[test]
    fn empty_rate_limits_object_is_some_with_no_windows() {
        let body = r#"{"rate_limits":{}}"#;
        let rl = parse(body).unwrap().rate_limits.unwrap();
        assert!(rl.five_hour.is_none());
        assert!(rl.seven_day.is_none());
    }

    #[test]
    fn parses_model_and_context_window() {
        // FULL already carries model.display_name "Opus" and
        // context_window.used_percentage 4.2.
        let s = parse(FULL).expect("parses");
        assert_eq!(s.model_display_name.as_deref(), Some("Opus"));
        assert!((s.context_used_percent.unwrap() - 4.2).abs() < 1e-4);
    }

    #[test]
    fn model_and_context_absent_are_none() {
        let s = parse(r#"{"version":"2.1.140"}"#).expect("parses");
        assert!(s.model_display_name.is_none());
        assert!(s.context_used_percent.is_none());
    }

    #[test]
    fn null_optional_inner_fields_degrade_to_none() {
        // model.display_name and context_window.used_percentage are optional
        // display fields: an explicit null degrades to None (plain Option),
        // NOT SchemaDrift. The counterpart to
        // explicit_null_required_field_is_schema_drift_not_dropped, which pins
        // the opposite contract for required numerics.
        let body =
            r#"{"model":{"id":"x","display_name":null},"context_window":{"used_percentage":null}}"#;
        let s = parse(body).expect("optional inner nulls must not error");
        assert!(s.model_display_name.is_none());
        assert!(s.context_used_percent.is_none());
    }

    #[test]
    fn wrong_type_optional_cosmetic_fields_degrade_not_fail() {
        // A WRONG-TYPE in an optional cosmetic field (display_name as a number,
        // used_percentage as a string) must NOT fail the whole parse - that
        // would blank the entire statusline, including the valuable rate-limit
        // data, over a display-only drift. It drops just that field to None
        // while the rest of the payload survives. Distinct from the rate-limit
        // windows, where a wrong-typed required numeric IS SchemaDrift.
        let body = r#"{
          "model":{"display_name":42},
          "context_window":{"used_percentage":"83%"},
          "rate_limits":{"five_hour":{"used_percentage":13.0,"resets_at":1747650600}},
          "cost":{"total_cost_usd":2.5}
        }"#;
        let s = parse(body).expect("wrong-typed cosmetic fields must not fail the payload");
        assert!(
            s.model_display_name.is_none(),
            "wrong-type display_name degrades to None"
        );
        assert!(
            s.context_used_percent.is_none(),
            "wrong-type used_percentage degrades to None"
        );
        assert!(
            s.rate_limits.unwrap().five_hour.is_some(),
            "rate-limit data survives a cosmetic-field drift"
        );
        assert_eq!(s.session_cost_micro_usd, Some(2_500_000), "cost survives");
    }
}
