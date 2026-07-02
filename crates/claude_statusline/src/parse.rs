use chrono::{TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::errors::StatuslineError;
use crate::types::{RateLimits, RateWindow, StatuslineSnapshot};

#[derive(Debug, Deserialize)]
struct RawRoot {
    version: Option<String>,
    cost: Option<RawCost>,
    // Raw JSON object: each key is a candidate rate-limit window (e.g.
    // "five_hour", "seven_day", or any future key). Kept as a generic map
    // rather than a fixed struct so an unrecognized key parses instead of
    // being dropped by serde before `parse()` ever sees it - the same
    // problem `anthropic_oauth::client.rs::parse_response` already solves
    // for the OAuth `/api/oauth/usage` cadence bars.
    rate_limits: Option<Map<String, Value>>,
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
struct RawWindow {
    // `#[serde(default)]` + `deserialize_non_null` make an ABSENT field degrade
    // to `None` (forward-compat for a future field rename - see
    // `window_or_drop`), while a present `null` or wrong-TYPE value still errors
    // out of `serde_json::from_value` (caught in `parse_rate_limits` and turned
    // into a whole-payload `SchemaDrift`). Forward-compat covers renames, not
    // corrupt/null values for a required numeric field.
    #[serde(default, deserialize_with = "deserialize_non_null")]
    used_percentage: Option<f32>,
    /// Unix epoch SECONDS (per the documented schema).
    #[serde(default, deserialize_with = "deserialize_non_null")]
    resets_at: Option<i64>,
}

/// Deserialize a *present* field but treat an explicit `null` as an error, not
/// `None`. Paired with `#[serde(default)]` on the field: an ABSENT field → `None`
/// (this fn isn't called), a present `null` → the inner `T::deserialize` fails
/// (a required numeric can't be null) → the caller turns that into `SchemaDrift`,
/// a present value → `Some(value)`. So the degrade-to-`None` forward-compat
/// applies only to genuinely-missing fields, never to null/corrupt values.
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

    let rate_limits = raw.rate_limits.map(parse_rate_limits).transpose()?;

    Ok(StatuslineSnapshot {
        rate_limits,
        session_cost_micro_usd,
        claude_code_version: raw.version,
        model_display_name: raw.model.and_then(|m| m.display_name),
        context_used_percent: raw.context_window.and_then(|c| c.used_percentage),
    })
}

/// Parse the `rate_limits` object into a generic [`RateLimits`]. Every key is
/// a candidate window - not just `five_hour`/`seven_day` - so a future
/// Anthropic addition (e.g. a per-model weekly bucket) parses instead of
/// silently dropping.
///
/// Per-key semantics, preserved from the original fixed-field parser:
/// - a key is simply absent from the object -> no window (nothing to do here;
///   the map only ever contains present keys)
/// - value is JSON `null` -> treated as absent, skipped
/// - value present, a required field explicitly `null` or wrong-typed ->
///   the WHOLE payload fails as `SchemaDrift` (corruption, not absence)
/// - value present, missing `used_percentage` or `resets_at` -> that window
///   is dropped (`warn!`), the rest of the payload survives
/// - value present, `resets_at` out of chrono's representable range -> that
///   window is dropped (`warn!`), the rest of the payload survives
/// - value present, well-formed, any key -> a `RateWindow`, with a curated
///   label for `five_hour`/`seven_day` and a titlecased fallback otherwise
fn parse_rate_limits(obj: Map<String, Value>) -> Result<RateLimits, StatuslineError> {
    let mut windows = Vec::new();
    for (key, value) in obj {
        if value.is_null() {
            continue;
        }
        let raw: RawWindow = serde_json::from_value(value).map_err(|e| {
            // A required field was explicitly null or wrong-typed inside a
            // PRESENT window object: corruption, not absence. Fails the whole
            // payload rather than silently dropping this window - matches the
            // pre-generalization contract for five_hour/seven_day.
            StatuslineError::SchemaDrift {
                message: format!("rate_limits.{key}: {e}"),
            }
        })?;
        if let Some(window) = window_or_drop(&key, raw) {
            windows.push(window);
        }
    }
    windows.sort_by(|a, b| {
        window_sort_key(&a.key)
            .cmp(&window_sort_key(&b.key))
            .then_with(|| a.key.cmp(&b.key))
    });
    Ok(RateLimits { windows })
}

/// Convert a raw window block to a `RateWindow`, or drop it (`None`).
///
/// A block that is *present but missing* a required field (e.g. a future
/// statusLine field rename) degrades to `None` - that one window is dropped
/// rather than failing the whole payload, so a single rename can't blank the
/// user's shell prompt or take out the other windows + the session cost. The
/// same drop-with-warn applies to a `resets_at` outside chrono's representable
/// range, instead of rewriting it to the Unix epoch.
///
/// A dropped window is logged at `warn!`. `balanze-cli` installs a `tracing`
/// subscriber for every subcommand (including `statusline`), so the warning is
/// observable on stderr when the env filter enables warn-level (e.g.
/// `RUST_LOG=warn`); it goes to stderr, never the statusLine's stdout, so it
/// can't pollute the prompt. With no subscriber installed (a library embedding
/// the parser) it is a silent no-op.
fn window_or_drop(key: &str, raw: RawWindow) -> Option<RateWindow> {
    match (raw.used_percentage, raw.resets_at) {
        (Some(used_percent), Some(secs)) => {
            // An out-of-range epoch (beyond chrono's ~±262,000-year span) is
            // corrupt wire data: drop the window visibly rather than rewrite
            // it to a plausible-looking 1970-01-01.
            let Some(resets_at) = Utc.timestamp_opt(secs, 0).single() else {
                tracing::warn!(
                    "claude_statusline: dropping `{key}` rate-limit window - \
                     out-of-range resets_at={secs}"
                );
                return None;
            };
            Some(RateWindow {
                key: key.to_string(),
                label: window_label(key),
                used_percent,
                resets_at,
            })
        }
        _ => {
            tracing::warn!(
                "claude_statusline: dropping `{key}` rate-limit window - present but \
                 missing used_percentage/resets_at (statusLine schema drift?)"
            );
            None
        }
    }
}

/// Human-friendly label for a rate-limit window key. Curated for the two
/// windows Claude Code has always sent; anything else gets a titlecased
/// fallback so a future addition still renders sensibly. Deliberately NOT
/// shared with `anthropic_oauth::client.rs::cadence_label()` - the two crates
/// solve the same small problem independently rather than taking on a
/// cross-crate dependency for ~15 lines (design spec D3).
fn window_label(key: &str) -> String {
    match key {
        "five_hour" => "5-hour".to_string(),
        "seven_day" => "7-day".to_string(),
        other => titlecase_key(other),
    }
}

fn window_sort_key(key: &str) -> u8 {
    match key {
        "five_hour" => 0,
        "seven_day" => 1,
        _ => 100,
    }
}

fn titlecase_key(key: &str) -> String {
    key.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        let fh = rl.five_hour().expect("five_hour");
        assert!((fh.used_percent - 13.0).abs() < 1e-4);
        assert_eq!(fh.resets_at.timestamp(), 1747650600);
        assert!((rl.seven_day().unwrap().used_percent - 44.0).abs() < 1e-4);
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
    fn null_window_value_treated_as_absent() {
        // A key present with JSON `null` (e.g. a plan without a 7-day cap)
        // is treated the same as the key being absent entirely - distinct
        // from a null on a REQUIRED FIELD INSIDE a present window object,
        // which is corruption (see explicit_null_required_field_is_schema_drift_not_dropped).
        // The original fixed-field parser got this for free from serde's
        // `Option<T>` semantics; the generic map-based parser implements it
        // explicitly (`if value.is_null() { continue; }`), so it earns its
        // own test.
        let body = r#"{
          "cost":{"total_cost_usd":2.5},
          "rate_limits":{
            "five_hour":null,
            "seven_day":{"used_percentage":44.0,"resets_at":1747915200}
          }}"#;
        let s = parse(body).expect("a null window value must not fail the payload");
        let rl = s.rate_limits.expect("rate_limits present");
        assert!(rl.five_hour().is_none(), "null value ⇒ treated as absent");
        assert!(rl.seven_day().is_some(), "seven_day intact ⇒ preserved");
        assert_eq!(rl.windows.len(), 1);
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
        // rename) drops just that window to None - the OTHER window AND the
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
            rl.five_hour().is_none(),
            "five_hour missing resets_at ⇒ dropped to None"
        );
        assert!(rl.seven_day().is_some(), "seven_day intact ⇒ preserved");
        assert_eq!(
            s.session_cost_micro_usd,
            Some(2_500_000),
            "session cost still parsed"
        );
    }

    #[test]
    fn explicit_null_required_field_is_schema_drift_not_dropped() {
        // An explicit `null` for a required numeric is corruption, not a missing
        // field - it must hard-error (SchemaDrift), NOT silently drop the window
        // (which `Option<T>` would do by mapping null → None). Distinguishes
        // absent (degrade) from present-null (error).
        let body = r#"{"rate_limits":{"five_hour":{"used_percentage":1.0,"resets_at":null}}}"#;
        match parse(body) {
            Err(StatuslineError::SchemaDrift { .. }) => {}
            other => panic!("expected SchemaDrift for explicit null, got {other:?}"),
        }
    }

    #[test]
    fn explicit_null_required_field_on_unknown_key_is_also_schema_drift() {
        // The strict "present-null-on-required-field is corruption" rule must
        // generalize to any key, not just the two originally-hardcoded ones -
        // otherwise a new window's corrupt data would silently vanish instead
        // of surfacing.
        let body =
            r#"{"rate_limits":{"seven_day_fable":{"used_percentage":1.0,"resets_at":null}}}"#;
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
            rl.five_hour().is_none(),
            "out-of-range resets_at ⇒ window dropped, not epoch-rewritten"
        );
        assert!(rl.seven_day().is_some(), "seven_day intact ⇒ preserved");
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
        assert!(rl.five_hour().is_some());
        assert!(rl.seven_day().is_none());
    }

    #[test]
    fn empty_rate_limits_object_is_some_with_no_windows() {
        let body = r#"{"rate_limits":{}}"#;
        let rl = parse(body).unwrap().rate_limits.unwrap();
        assert!(rl.five_hour().is_none());
        assert!(rl.seven_day().is_none());
        assert!(rl.windows.is_empty());
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
            s.rate_limits.unwrap().five_hour().is_some(),
            "rate-limit data survives a cosmetic-field drift"
        );
        assert_eq!(s.session_cost_micro_usd, Some(2_500_000), "cost survives");
    }

    #[test]
    fn unknown_window_key_parses_with_titlecased_fallback_label() {
        // Anthropic could add a new named window at any time (e.g. a
        // per-model weekly bucket). It must parse, not vanish.
        let body = r#"{
          "rate_limits":{
            "seven_day_fable":{"used_percentage":0.0,"resets_at":1751925600}
          }}"#;
        let s = parse(body).expect("parses");
        let rl = s.rate_limits.expect("rate_limits present");
        assert_eq!(rl.windows.len(), 1);
        assert_eq!(rl.windows[0].key, "seven_day_fable");
        assert_eq!(rl.windows[0].label, "Seven Day Fable");
        assert_eq!(rl.windows[0].used_percent, 0.0);
    }

    #[test]
    fn three_windows_all_present_five_hour_and_seven_day_first() {
        let body = r#"{
          "rate_limits":{
            "seven_day_fable":{"used_percentage":0.0,"resets_at":1751925600},
            "seven_day":{"used_percentage":2.0,"resets_at":1751925600},
            "five_hour":{"used_percentage":17.0,"resets_at":1751896200}
          }}"#;
        let s = parse(body).expect("parses");
        let rl = s.rate_limits.expect("rate_limits present");
        assert_eq!(rl.windows.len(), 3);
        let keys: Vec<&str> = rl.windows.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["five_hour", "seven_day", "seven_day_fable"],
            "known keys sort first (five_hour, seven_day), unknown after"
        );
        assert_eq!(rl.five_hour().unwrap().used_percent, 17.0);
        assert_eq!(rl.seven_day().unwrap().used_percent, 2.0);
    }
}
