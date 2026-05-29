use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use crate::errors::StatuslineError;
use crate::types::{RateLimits, RateWindow, StatuslineSnapshot};

#[derive(Debug, Deserialize)]
struct RawRoot {
    version: Option<String>,
    cost: Option<RawCost>,
    rate_limits: Option<RawRateLimits>,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    total_cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawRateLimits {
    five_hour: Option<RawWindow>,
    seven_day: Option<RawWindow>,
}

#[derive(Debug, Deserialize)]
struct RawWindow {
    // Optional so a partial window-shape change (a future field rename) degrades
    // THIS window to `None` rather than failing the whole payload — see
    // `window_or_drop`. A present-but-wrong-TYPE value still errors as
    // `SchemaDrift` during deserialization.
    used_percentage: Option<f32>,
    /// Unix epoch SECONDS (per the documented schema).
    resets_at: Option<i64>,
}

/// Parse the Claude Code statusLine stdin payload. Pure, infallible except
/// for invalid JSON or a present-but-wrong-shape required subfield. Unknown
/// fields are tolerated; absent optional blocks become `None`.
pub fn parse(input: &str) -> Result<StatuslineSnapshot, StatuslineError> {
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
    })
}

/// Convert a raw window block to a `RateWindow`, or drop it (`None`).
///
/// A block that is *present but missing* a required field (e.g. a future
/// statusLine field rename) degrades to `None` — that one window is dropped
/// rather than failing the whole payload, so a single rename can't blank the
/// user's shell prompt or take out the other window + the session cost. A
/// present-but-WRONG-TYPE field still surfaces as `SchemaDrift` upstream (it
/// fails `serde` deserialization before we get here). A dropped window is logged
/// at `warn!` so the drift stays visible to operators (a no-op when no tracing
/// subscriber is installed — e.g. the bare `balanze-cli statusline` shell
/// command — so it never pollutes the prompt).
fn window_or_drop(name: &str, raw: Option<RawWindow>) -> Option<RateWindow> {
    let raw = raw?;
    match (raw.used_percentage, raw.resets_at) {
        (Some(used_percent), Some(secs)) => {
            let resets_at: DateTime<Utc> = Utc
                .timestamp_opt(secs, 0)
                .single()
                .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
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
}
