# Statusline generic rate windows - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generalize `claude_statusline::RateLimits` from a fixed `{ five_hour, seven_day }` struct to a generic `{ windows: Vec<RateWindow> }` + named accessors, so a new named rate-limit window (e.g. a per-model weekly bucket, mirroring what `anthropic_oauth` already handles for OAuth cadences) doesn't silently disappear on the statusLine ingestion path.

**Architecture:** Mirror `anthropic_oauth::ClaudeOAuthSnapshot`'s already-proven "generic storage + named accessors" shape. Change ripples mechanically through 4 downstream consumers (accessor method call instead of field access) plus one real behavioral change (`CardsView.svelte`'s statusline branch starts mapping every window instead of two hardcoded ones), a `SCHEMA_VERSION` bump for the on-disk IPC file, an additive CLI `--json` schema extension, and two `docs/ARCHITECTURE.md` edits.

**Tech Stack:** Rust (serde, chrono, thiserror), TypeScript, Svelte 5.

**Design spec:** `docs/superpowers/specs/2026-07-02-statusline-generic-rate-windows-design.md`

---

### Task 1: `claude_statusline` - generic RateLimits data model + parser

**Goal:** `RateWindow` gains `key`/`label`; `RateLimits` becomes `{ windows: Vec<RateWindow> }` with `.five_hour()`/`.seven_day()` accessors; `parse.rs` generalizes to accept any named window in the `rate_limits` object while preserving every existing drop/degrade/schema-drift rule; `SCHEMA_VERSION` bumps to 2.

**Files:**
- Modify: `crates/claude_statusline/src/types.rs`
- Modify: `crates/claude_statusline/src/parse.rs`
- Modify: `crates/claude_statusline/src/payload.rs`
- Modify: `crates/claude_statusline/tests/real_payload.rs`

**Acceptance Criteria:**
- [ ] `RateLimits::five_hour()` / `.seven_day()` return the matching window by key, `None` if absent
- [ ] A `rate_limits` key other than `five_hour`/`seven_day` (e.g. `seven_day_fable`) parses into a `RateWindow` in `.windows`, with a titlecased fallback `label`
- [ ] All 5 existing per-window semantics (absent key, null-as-absent, drop-with-warn on missing field, hard `SchemaDrift` on explicit-null-on-required-field, drop-with-warn on out-of-range `resets_at`) hold for an unknown key exactly as they did for `five_hour`/`seven_day`
- [ ] `SCHEMA_VERSION` is `2`

**Verify:** `cargo test -p claude_statusline` -> all tests pass

**Steps:**

- [ ] **Step 1: Rewrite `types.rs` - data model + accessor tests**

Replace the full contents of `crates/claude_statusline/src/types.rs` with:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One server-authoritative subscription window from the statusLine feed.
/// `anthropic_oauth::CadenceBar`'s analogous fields are `key`/`display_label`/
/// `utilization_percent`; `RateWindow` uses the shorter `used_percent` and
/// `resets_at: DateTime<Utc>`. The watcher aligns the two sources (a small
/// field-name mapping step).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    /// Raw wire key from the statusLine `rate_limits` object (e.g.
    /// `"five_hour"`, `"seven_day"`, or any future key Claude Code adds).
    pub key: String,
    /// Human-friendly display label, synthesized at parse time. Known keys
    /// map to curated strings (`"5-hour"`, `"7-day"`); unknown keys titlecase
    /// the raw key so a future addition still renders sensibly.
    pub label: String,
    pub used_percent: f32,
    pub resets_at: DateTime<Utc>,
}

/// All rate-limit windows from one statusLine payload. Generic over however
/// many named windows Claude Code reports - not just `five_hour`/`seven_day` -
/// mirroring `anthropic_oauth::ClaudeOAuthSnapshot`'s `cadences: Vec<CadenceBar>`
/// + named-accessor shape for the same problem (an arbitrary, growing set of
/// named usage windows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    pub windows: Vec<RateWindow>,
}

impl RateLimits {
    /// The 5-hour session window, if present.
    pub fn five_hour(&self) -> Option<&RateWindow> {
        self.windows.iter().find(|w| w.key == "five_hour")
    }

    /// The 7-day "all models" window, if present.
    pub fn seven_day(&self) -> Option<&RateWindow> {
        self.windows.iter().find(|w| w.key == "seven_day")
    }
}

/// Parsed statusLine payload. `None` fields = "not present in this payload"
/// (e.g. `rate_limits` is Pro/Max-only and only after the first API
/// response). `session_cost_micro_usd` is a Claude-side SESSION ESTIMATE
/// (i64 micro-USD, AGENTS.md §2.1) - a distinct cost tier, never conflated
/// with the JSONL list-price estimate or the real `extra_usage` overage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatuslineSnapshot {
    pub rate_limits: Option<RateLimits>,
    pub session_cost_micro_usd: Option<i64>,
    pub claude_code_version: Option<String>,
    /// Human model name from `model.display_name` (e.g. "Opus 4.7 (1M context)").
    pub model_display_name: Option<String>,
    /// Context-window utilization percent from `context_window.used_percentage`.
    pub context_used_percent: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn five_hour_returns_the_five_hour_window() {
        let rl = RateLimits {
            windows: vec![
                RateWindow {
                    key: "seven_day".to_string(),
                    label: "7-day".to_string(),
                    used_percent: 10.0,
                    resets_at: ts("2026-05-20T00:00:00Z"),
                },
                RateWindow {
                    key: "five_hour".to_string(),
                    label: "5-hour".to_string(),
                    used_percent: 42.0,
                    resets_at: ts("2026-05-15T18:00:00Z"),
                },
            ],
        };
        let w = rl.five_hour().expect("five_hour present");
        assert_eq!(w.used_percent, 42.0);
    }

    #[test]
    fn five_hour_is_none_when_absent() {
        let rl = RateLimits {
            windows: vec![RateWindow {
                key: "seven_day".to_string(),
                label: "7-day".to_string(),
                used_percent: 10.0,
                resets_at: ts("2026-05-20T00:00:00Z"),
            }],
        };
        assert!(rl.five_hour().is_none());
    }

    #[test]
    fn seven_day_returns_the_seven_day_window() {
        let rl = RateLimits {
            windows: vec![RateWindow {
                key: "seven_day".to_string(),
                label: "7-day".to_string(),
                used_percent: 88.0,
                resets_at: ts("2026-05-20T00:00:00Z"),
            }],
        };
        let w = rl.seven_day().expect("seven_day present");
        assert_eq!(w.used_percent, 88.0);
    }

    #[test]
    fn seven_day_is_none_when_absent() {
        let rl = RateLimits { windows: vec![] };
        assert!(rl.seven_day().is_none());
    }

    #[test]
    fn an_unknown_key_window_is_reachable_only_via_windows() {
        // Windows beyond five_hour/seven_day have no named accessor - they're
        // only reachable via the generic `windows` list. Pins that the
        // accessors don't accidentally act as a filter that hides them.
        let rl = RateLimits {
            windows: vec![RateWindow {
                key: "seven_day_fable".to_string(),
                label: "Seven Day Fable".to_string(),
                used_percent: 0.0,
                resets_at: ts("2026-07-07T23:00:00Z"),
            }],
        };
        assert!(rl.five_hour().is_none());
        assert!(rl.seven_day().is_none());
        assert_eq!(rl.windows.len(), 1);
        assert_eq!(rl.windows[0].key, "seven_day_fable");
    }
}
```

- [ ] **Step 2: Run the new type-level tests**

Run: `cargo test -p claude_statusline types::tests`
Expected: 5 tests pass (`five_hour_returns_the_five_hour_window`, `five_hour_is_none_when_absent`, `seven_day_returns_the_seven_day_window`, `seven_day_is_none_when_absent`, `an_unknown_key_window_is_reachable_only_via_windows`)

- [ ] **Step 3: Rewrite `parse.rs` - generalize the parser**

Replace the full contents of `crates/claude_statusline/src/parse.rs` with:

```rust
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
                "claude_statusline: dropping `{key}` rate-limit window — present but \
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
```

- [ ] **Step 4: Bump `SCHEMA_VERSION` in `payload.rs`**

In `crates/claude_statusline/src/payload.rs`, change:

```rust
pub const SCHEMA_VERSION: u8 = 1;
```

to:

```rust
pub const SCHEMA_VERSION: u8 = 2;
```

And update the test module's two literal assertions - change:

```rust
    #[test]
    fn schema_version_is_one() {
        let captured_at = Utc.with_ymd_and_hms(2026, 5, 21, 12, 0, 0).unwrap();
        let p = StatuslineFilePayload::new(sample_snapshot(), captured_at);
        assert_eq!(p.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn new_stamps_schema_version() {
        let captured_at = Utc.with_ymd_and_hms(2026, 5, 21, 0, 0, 0).unwrap();
        let p = StatuslineFilePayload::new(sample_snapshot(), captured_at);
        assert_eq!(p.schema_version, 1);
        assert_eq!(p.captured_at, captured_at);
    }
```

to:

```rust
    #[test]
    fn schema_version_is_two() {
        let captured_at = Utc.with_ymd_and_hms(2026, 5, 21, 12, 0, 0).unwrap();
        let p = StatuslineFilePayload::new(sample_snapshot(), captured_at);
        assert_eq!(p.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 2);
    }

    #[test]
    fn new_stamps_schema_version() {
        let captured_at = Utc.with_ymd_and_hms(2026, 5, 21, 0, 0, 0).unwrap();
        let p = StatuslineFilePayload::new(sample_snapshot(), captured_at);
        assert_eq!(p.schema_version, 2);
        assert_eq!(p.captured_at, captured_at);
    }
```

- [ ] **Step 5: Update the real-payload fixture test**

In `crates/claude_statusline/tests/real_payload.rs`, change:

```rust
    let fh = rl.five_hour.expect("five_hour window present");
    assert!((fh.used_percent - 45.0).abs() < 1e-4);
    assert_eq!(fh.resets_at.timestamp(), 1779209400);
    let sd = rl.seven_day.expect("seven_day window present");
    assert!((sd.used_percent - 54.0).abs() < 1e-4);
    assert_eq!(sd.resets_at.timestamp(), 1779458400);
```

to:

```rust
    let fh = rl.five_hour().expect("five_hour window present");
    assert!((fh.used_percent - 45.0).abs() < 1e-4);
    assert_eq!(fh.resets_at.timestamp(), 1779209400);
    let sd = rl.seven_day().expect("seven_day window present");
    assert!((sd.used_percent - 54.0).abs() < 1e-4);
    assert_eq!(sd.resets_at.timestamp(), 1779458400);
```

- [ ] **Step 6: Run the full crate test suite**

Run: `cargo test -p claude_statusline`
Expected: all tests pass (20 in `parse.rs`, 5 in `types.rs`, 3 in `payload.rs`, plus `file_io.rs`/`wiring.rs` tests unaffected, plus `real_payload.rs`)

- [ ] **Step 7: Commit**

```bash
git add crates/claude_statusline/src/types.rs crates/claude_statusline/src/parse.rs crates/claude_statusline/src/payload.rs crates/claude_statusline/tests/real_payload.rs
git commit -m "feat(statusline): generalize RateLimits to an arbitrary window list

RateLimits becomes { windows: Vec<RateWindow> } with .five_hour()/.seven_day()
accessors, mirroring anthropic_oauth::ClaudeOAuthSnapshot's cadences +
named-accessor shape. parse.rs generalizes to accept any key in the
rate_limits object while preserving every existing per-window semantic
(absent/null-as-absent/drop-with-warn/hard-schema-drift/out-of-range-drop).
SCHEMA_VERSION bumps to 2 for the on-disk statusline.snapshot.json shape
change."
```

---

### Task 2: `statusline_render` - accessor swap + test fixtures

**Goal:** `render_usage` and its test fixtures move from `RateLimits { five_hour, seven_day }` field access/construction to the new accessor methods / `windows` list, with the rendered terminal text byte-for-byte unchanged.

**Files:**
- Modify: `crates/statusline_render/src/render.rs`

**Acceptance Criteria:**
- [ ] `render_usage` compiles against the new `RateLimits` shape
- [ ] Every existing `statusline_render` test still passes with unchanged assertions on the rendered string

**Verify:** `cargo test -p statusline_render` -> all tests pass; `renders_default_layout_plain` still asserts the exact same substrings as before

**Steps:**

- [ ] **Step 1: Update production code**

In `crates/statusline_render/src/render.rs`, change:

```rust
fn render_usage(input: &RenderInput) -> Option<String> {
    let rl = input.snapshot.rate_limits.as_ref()?;
    let c = &input.config.segments.usage;
    let mut windows: Vec<String> = Vec::new();
    if let Some(w) = &rl.five_hour {
        windows.push(render_window("⌛5h", w, Duration::hours(5), c, input));
    }
    if let Some(w) = &rl.seven_day {
        windows.push(render_window("📅7d", w, Duration::days(7), c, input));
    }
    if windows.is_empty() {
        None
    } else {
        Some(windows.join(" "))
    }
}
```

to:

```rust
fn render_usage(input: &RenderInput) -> Option<String> {
    let rl = input.snapshot.rate_limits.as_ref()?;
    let c = &input.config.segments.usage;
    let mut windows: Vec<String> = Vec::new();
    if let Some(w) = rl.five_hour() {
        windows.push(render_window("⌛5h", w, Duration::hours(5), c, input));
    }
    if let Some(w) = rl.seven_day() {
        windows.push(render_window("📅7d", w, Duration::days(7), c, input));
    }
    if windows.is_empty() {
        None
    } else {
        Some(windows.join(" "))
    }
}
```

(Only two field accesses became method calls. The rendered text stays fixed at exactly these two segments regardless of how many windows the data model now carries - design spec D4.)

- [ ] **Step 2: Update the `snap()` test helper**

In `crates/statusline_render/src/render.rs`'s test module, change:

```rust
    fn snap() -> claude_statusline::StatuslineSnapshot {
        claude_statusline::StatuslineSnapshot {
            rate_limits: Some(claude_statusline::RateLimits {
                five_hour: Some(claude_statusline::RateWindow {
                    used_percent: 82.0,
                    resets_at: now() + chrono::Duration::minutes(83),
                }),
                seven_day: Some(claude_statusline::RateWindow {
                    used_percent: 88.0,
                    resets_at: now() + chrono::Duration::days(5),
                }),
            }),
            session_cost_micro_usd: Some(2_500_000),
            claude_code_version: None,
            model_display_name: Some("Opus".to_string()),
            context_used_percent: Some(42.0),
        }
    }
```

to:

```rust
    fn snap() -> claude_statusline::StatuslineSnapshot {
        claude_statusline::StatuslineSnapshot {
            rate_limits: Some(claude_statusline::RateLimits {
                windows: vec![
                    claude_statusline::RateWindow {
                        key: "five_hour".to_string(),
                        label: "5-hour".to_string(),
                        used_percent: 82.0,
                        resets_at: now() + chrono::Duration::minutes(83),
                    },
                    claude_statusline::RateWindow {
                        key: "seven_day".to_string(),
                        label: "7-day".to_string(),
                        used_percent: 88.0,
                        resets_at: now() + chrono::Duration::days(5),
                    },
                ],
            }),
            session_cost_micro_usd: Some(2_500_000),
            claude_code_version: None,
            model_display_name: Some("Opus".to_string()),
            context_used_percent: Some(42.0),
        }
    }
```

- [ ] **Step 3: Update the four test-side mutation sites**

In `crates/statusline_render/src/render.rs`'s test module, there are 4 places that mutate `s.rate_limits.as_mut().unwrap().five_hour.as_mut().unwrap()...` (and one that also mutates `seven_day`). Each becomes a lookup into `.windows` by key. Change each occurrence of this pattern:

```rust
        s.rate_limits
            .as_mut()
            .unwrap()
            .five_hour
            .as_mut()
            .unwrap()
            .used_percent = 82.5;
```

(appears in `display_percent_uses_round_half_away_matching_tone`, `color_true_wraps_toned_segments` with `95.0`, and `color_true_wraps_warn_tone_with_warn_style` with `75.0`) to:

```rust
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 82.5;
```

(keeping each test's own specific value: `82.5`, `95.0`, `75.0` respectively).

And in `no_pace_arrow_right_after_reset`, change:

```rust
        {
            let rl = s.rate_limits.as_mut().unwrap();
            rl.five_hour.as_mut().unwrap().resets_at = n + chrono::Duration::hours(5);
            rl.seven_day.as_mut().unwrap().resets_at = n + chrono::Duration::days(7);
        }
```

to:

```rust
        {
            let rl = s.rate_limits.as_mut().unwrap();
            rl.windows
                .iter_mut()
                .find(|w| w.key == "five_hour")
                .unwrap()
                .resets_at = n + chrono::Duration::hours(5);
            rl.windows
                .iter_mut()
                .find(|w| w.key == "seven_day")
                .unwrap()
                .resets_at = n + chrono::Duration::days(7);
        }
```

- [ ] **Step 4: Run the full crate test suite**

Run: `cargo test -p statusline_render`
Expected: all tests pass, including `renders_default_layout_plain`, `display_percent_uses_round_half_away_matching_tone`, `no_pace_arrow_right_after_reset`, `color_true_wraps_toned_segments`, `color_true_wraps_warn_tone_with_warn_style` - none of their string assertions change

- [ ] **Step 5: Commit**

```bash
git add crates/statusline_render/src/render.rs
git commit -m "refactor(statusline): adapt render_usage to the generic RateLimits shape

Mechanical accessor swap (rl.five_hour -> rl.five_hour()); rendered
terminal text is unchanged, still exactly the 5h + 7d segments."
```

---

### Task 3: `src-tauri` tauri_sink - accessor swap

**Goal:** `worst_utilization` and `has_quota_data` use the new accessor methods instead of field access. No behavior change, no test changes (neither function has existing test coverage via the statusline path - both are only exercised via the OAuth path today).

**Files:**
- Modify: `src-tauri/src/tauri_sink.rs`

**Acceptance Criteria:**
- [ ] `src-tauri` compiles against the new `RateLimits` shape
- [ ] Existing `tauri_sink.rs` tests (`worst_util_picks_max_across_sources`, `empty_snapshot_has_no_quota_data`, `oauth_cadence_counts_as_quota_data`, `empty_snapshot_paints_neutral_not_green`) still pass unchanged

**Verify:** `cargo build -p balanze --manifest-path src-tauri/Cargo.toml` compiles; `cargo test -p balanze --manifest-path src-tauri/Cargo.toml` -> existing tests still pass

**Steps:**

- [ ] **Step 1: Update `worst_utilization`**

In `src-tauri/src/tauri_sink.rs`, change:

```rust
    if let Some(sl) = &s.claude_statusline {
        if let Some(rl) = &sl.payload.rate_limits {
            if let Some(w) = &rl.five_hour {
                worst = worst.max(w.used_percent);
            }
            if let Some(w) = &rl.seven_day {
                worst = worst.max(w.used_percent);
            }
        }
    }
```

to:

```rust
    if let Some(sl) = &s.claude_statusline {
        if let Some(rl) = &sl.payload.rate_limits {
            if let Some(w) = rl.five_hour() {
                worst = worst.max(w.used_percent);
            }
            if let Some(w) = rl.seven_day() {
                worst = worst.max(w.used_percent);
            }
        }
    }
```

- [ ] **Step 2: Update `has_quota_data`**

In the same file, change:

```rust
    let statusline = s
        .claude_statusline
        .as_ref()
        .and_then(|sl| sl.payload.rate_limits.as_ref())
        .is_some_and(|rl| rl.five_hour.is_some() || rl.seven_day.is_some());
```

to:

```rust
    let statusline = s
        .claude_statusline
        .as_ref()
        .and_then(|sl| sl.payload.rate_limits.as_ref())
        .is_some_and(|rl| rl.five_hour().is_some() || rl.seven_day().is_some());
```

- [ ] **Step 3: Build and run existing tests**

Run: `cargo test -p balanze --manifest-path src-tauri/Cargo.toml`
Expected: all existing tests pass (none construct `RateLimits` directly - both `worst_utilization`/`has_quota_data` tests use the OAuth `oauth_with_util` helper, unaffected by this change)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/tauri_sink.rs
git commit -m "refactor(statusline): adapt tauri_sink to the generic RateLimits shape

Mechanical accessor swap in worst_utilization/has_quota_data. No
behavior change."
```

---

### Task 4: `balanze_cli` json_output - accessor swap + new `windows` field

**Goal:** `JsonClaudeStatusline` keeps `five_hour`/`seven_day` (now carrying `key`/`label` too) and gains a new `windows: Vec<JsonRateWindow>` field mirroring the existing `claude_oauth.cadences` field, giving the CLI `--json` output full parity with the OAuth path.

**Files:**
- Modify: `crates/balanze_cli/src/json_output.rs`

**Acceptance Criteria:**
- [ ] `JsonRateWindow` carries `key`, `label`, `used_percent`, `resets_at`
- [ ] `claude_statusline.windows` in the CLI `--json` output contains every window from the snapshot, in the same order `claude_statusline::parse` produced them
- [ ] `claude_statusline.five_hour`/`.seven_day` are unchanged in position and still null when absent
- [ ] A new test exercises a populated `rate_limits` with 3 windows (including one unrecognized key) and asserts all three appear in `windows`

**Verify:** `cargo test -p balanze_cli json_output` -> all tests pass, including the new one

**Steps:**

- [ ] **Step 1: Update `JsonRateWindow` and `JsonClaudeStatusline`**

In `crates/balanze_cli/src/json_output.rs`, change:

```rust
#[derive(Serialize)]
struct JsonClaudeStatusline {
    schema_version: u8,
    captured_at: DateTime<Utc>,
    five_hour: Option<JsonRateWindow>,
    seven_day: Option<JsonRateWindow>,
    /// Session cost converted from i64 micro-USD to f64 dollars at the JSON
    /// boundary (AGENTS.md §2.1 - f64 only at display).
    session_cost_usd: Option<f64>,
    claude_code_version: Option<String>,
    source: &'static str,
    confidence: &'static str,
}

#[derive(Serialize)]
struct JsonRateWindow {
    used_percent: f32,
    resets_at: DateTime<Utc>,
}

impl From<&StatuslineFilePayload> for JsonClaudeStatusline {
    fn from(p: &StatuslineFilePayload) -> Self {
        let snap = &p.payload;
        Self {
            schema_version: p.schema_version,
            captured_at: p.captured_at,
            five_hour: snap.rate_limits.as_ref().and_then(|rl| {
                rl.five_hour.as_ref().map(|w| JsonRateWindow {
                    used_percent: w.used_percent,
                    resets_at: w.resets_at,
                })
            }),
            seven_day: snap.rate_limits.as_ref().and_then(|rl| {
                rl.seven_day.as_ref().map(|w| JsonRateWindow {
                    used_percent: w.used_percent,
                    resets_at: w.resets_at,
                })
            }),
            session_cost_usd: snap
                .session_cost_micro_usd
                .map(|micro| micro as f64 / 1_000_000.0),
            claude_code_version: snap.claude_code_version.clone(),
            source: "claude_code_statusline",
            confidence: "estimate",
        }
    }
}
```

to:

```rust
#[derive(Serialize)]
struct JsonClaudeStatusline {
    schema_version: u8,
    captured_at: DateTime<Utc>,
    five_hour: Option<JsonRateWindow>,
    seven_day: Option<JsonRateWindow>,
    /// Every rate-limit window from the payload, in parser order (known keys
    /// first, unrecognized keys after). Mirrors `claude_oauth.cadences` -
    /// gives the CLI `--json` consumer full parity with the OAuth path
    /// instead of being capped at the two named windows above.
    windows: Vec<JsonRateWindow>,
    /// Session cost converted from i64 micro-USD to f64 dollars at the JSON
    /// boundary (AGENTS.md §2.1 - f64 only at display).
    session_cost_usd: Option<f64>,
    claude_code_version: Option<String>,
    source: &'static str,
    confidence: &'static str,
}

#[derive(Serialize)]
struct JsonRateWindow {
    key: String,
    label: String,
    used_percent: f32,
    resets_at: DateTime<Utc>,
}

impl From<&claude_statusline::RateWindow> for JsonRateWindow {
    fn from(w: &claude_statusline::RateWindow) -> Self {
        Self {
            key: w.key.clone(),
            label: w.label.clone(),
            used_percent: w.used_percent,
            resets_at: w.resets_at,
        }
    }
}

impl From<&StatuslineFilePayload> for JsonClaudeStatusline {
    fn from(p: &StatuslineFilePayload) -> Self {
        let snap = &p.payload;
        Self {
            schema_version: p.schema_version,
            captured_at: p.captured_at,
            five_hour: snap
                .rate_limits
                .as_ref()
                .and_then(|rl| rl.five_hour())
                .map(JsonRateWindow::from),
            seven_day: snap
                .rate_limits
                .as_ref()
                .and_then(|rl| rl.seven_day())
                .map(JsonRateWindow::from),
            windows: snap
                .rate_limits
                .as_ref()
                .map(|rl| rl.windows.iter().map(JsonRateWindow::from).collect())
                .unwrap_or_default(),
            session_cost_usd: snap
                .session_cost_micro_usd
                .map(|micro| micro as f64 / 1_000_000.0),
            claude_code_version: snap.claude_code_version.clone(),
            source: "claude_code_statusline",
            confidence: "estimate",
        }
    }
}
```

- [ ] **Step 2: Update the existing `claude_statusline_cell_shape` test**

In the test module, change:

```rust
    #[test]
    fn claude_statusline_cell_shape() {
        let mut s = Snapshot::empty(fixed_now());
        s.claude_statusline = Some(sample_statusline_payload());
        let v = render_to_value(&s, false);
        let cell = &v["claude_statusline"];
        assert_eq!(cell["schema_version"], 1);
        assert_eq!(cell["source"], "claude_code_statusline");
        assert_eq!(cell["confidence"], "estimate");
        // session_cost_usd: 3_420_000 µ$ → $3.42
        let cost = cell["session_cost_usd"].as_f64().unwrap();
        assert!((cost - 3.42).abs() < 1e-9, "session_cost_usd = {cost}");
        assert_eq!(cell["claude_code_version"], "v2.1.144");
        // rate_limits not set → five_hour and seven_day are null
        assert!(cell["five_hour"].is_null());
        assert!(cell["seven_day"].is_null());
    }
```

to:

```rust
    #[test]
    fn claude_statusline_cell_shape() {
        let mut s = Snapshot::empty(fixed_now());
        s.claude_statusline = Some(sample_statusline_payload());
        let v = render_to_value(&s, false);
        let cell = &v["claude_statusline"];
        assert_eq!(cell["schema_version"], 2);
        assert_eq!(cell["source"], "claude_code_statusline");
        assert_eq!(cell["confidence"], "estimate");
        // session_cost_usd: 3_420_000 µ$ → $3.42
        let cost = cell["session_cost_usd"].as_f64().unwrap();
        assert!((cost - 3.42).abs() < 1e-9, "session_cost_usd = {cost}");
        assert_eq!(cell["claude_code_version"], "v2.1.144");
        // rate_limits not set → five_hour, seven_day, and windows are empty
        assert!(cell["five_hour"].is_null());
        assert!(cell["seven_day"].is_null());
        assert_eq!(cell["windows"].as_array().unwrap().len(), 0);
    }
```

- [ ] **Step 3: Add a new test for a populated `rate_limits` with 3 windows**

Add this test in the same module, right after `claude_statusline_cell_shape`:

```rust
    #[test]
    fn claude_statusline_windows_carries_every_window_including_unknown_keys() {
        let n = fixed_now();
        let snap = StatuslineSnapshot {
            rate_limits: Some(claude_statusline::RateLimits {
                windows: vec![
                    claude_statusline::RateWindow {
                        key: "five_hour".to_string(),
                        label: "5-hour".to_string(),
                        used_percent: 17.0,
                        resets_at: n + chrono::Duration::hours(4),
                    },
                    claude_statusline::RateWindow {
                        key: "seven_day".to_string(),
                        label: "7-day".to_string(),
                        used_percent: 2.0,
                        resets_at: n + chrono::Duration::days(3),
                    },
                    claude_statusline::RateWindow {
                        key: "seven_day_fable".to_string(),
                        label: "Seven Day Fable".to_string(),
                        used_percent: 0.0,
                        resets_at: n + chrono::Duration::days(3),
                    },
                ],
            }),
            session_cost_micro_usd: Some(1_000_000),
            claude_code_version: Some("v2.1.144".to_string()),
            model_display_name: None,
            context_used_percent: None,
        };
        let mut s = Snapshot::empty(n);
        s.claude_statusline = Some(StatuslineFilePayload::new(snap, n));
        let v = render_to_value(&s, false);
        let cell = &v["claude_statusline"];

        assert_eq!(cell["five_hour"]["key"], "five_hour");
        assert_eq!(cell["five_hour"]["label"], "5-hour");
        assert_eq!(cell["seven_day"]["key"], "seven_day");

        let windows = cell["windows"].as_array().expect("windows array present");
        assert_eq!(windows.len(), 3, "all 3 windows present, not just the 2 named ones");
        let keys: Vec<&str> = windows.iter().map(|w| w["key"].as_str().unwrap()).collect();
        assert_eq!(keys, vec!["five_hour", "seven_day", "seven_day_fable"]);
        let fable = windows
            .iter()
            .find(|w| w["key"] == "seven_day_fable")
            .expect("unknown-key window present");
        assert_eq!(fable["label"], "Seven Day Fable");
    }
```

- [ ] **Step 4: Run the crate's json_output tests**

Run: `cargo test -p balanze_cli json_output`
Expected: all tests pass, including the 2 updated/new ones

- [ ] **Step 5: Run the full `balanze_cli` test suite**

Run: `cargo test -p balanze_cli`
Expected: all tests pass. Neither `integration_4quadrant.rs` nor `integration_statusline_self_compose.rs` references `claude_statusline`/`rate_limits`/`five_hour` (confirmed by grep during planning), so no integration fixture needs updating for this change.

- [ ] **Step 6: Commit**

```bash
git add crates/balanze_cli/src/json_output.rs
git commit -m "feat(cli): expose all statusline rate-limit windows in --json

claude_statusline.windows mirrors claude_oauth.cadences: the full list,
not just five_hour/seven_day. Additive - five_hour/seven_day keep their
position and shape (now with key/label too), so this doesn't need a
schema_version bump on the --json document (reserved for breaking
changes)."
```

---

### Task 5: Frontend TS types

**Goal:** `RateWindow`/`RateLimits` TypeScript interfaces mirror the new Rust shape.

**Files:**
- Modify: `src/lib/types/snapshot.ts`

**Acceptance Criteria:**
- [ ] `RateWindow` has `key`, `label`, `used_percent`, `resets_at`
- [ ] `RateLimits` has `windows: RateWindow[]`
- [ ] `bun run check` passes (svelte-check + tsc across all consumers)

**Verify:** `bun run check` -> no type errors

**Steps:**

- [ ] **Step 1: Update the types**

In `src/lib/types/snapshot.ts`, change:

```typescript
export interface RateWindow { used_percent: number; resets_at: string; }
export interface RateLimits { five_hour: RateWindow | null; seven_day: RateWindow | null; }
```

to:

```typescript
export interface RateWindow { key: string; label: string; used_percent: number; resets_at: string; }
export interface RateLimits { windows: RateWindow[]; }
```

- [ ] **Step 2: Run the type check (expect it to fail here - CardsView.svelte still references the old shape)**

Run: `bun run check`
Expected: FAIL - `src/lib/components/CardsView.svelte` errors on `rl.five_hour`/`rl.seven_day` (property does not exist on type `RateLimits`). This confirms the type change took effect; Task 6 fixes the consumer.

- [ ] **Step 3: Commit**

```bash
git add src/lib/types/snapshot.ts
git commit -m "refactor(statusline): mirror the generic RateLimits shape in TS types

RateWindow gains key/label; RateLimits becomes { windows: RateWindow[] }.
CardsView.svelte is updated in the next commit - bun run check is
expected to fail on this commit alone."
```

---

### Task 6: `CardsView.svelte` - generic statusline window mapping + gallery fixture

**Goal:** CardsView's statusline branch maps every window in `rate_limits.windows`, mirroring how its OAuth branch already maps every `cadences` entry. A new gallery fixture demonstrates 3 statusline-sourced windows (the codebase's states gallery currently has zero coverage of the statusline branch at all, even for today's 2-window case), giving `bun run tauri dev`/`/gallery` visual verification for the first time.

**Files:**
- Modify: `src/lib/components/CardsView.svelte`
- Modify: `src/lib/gallery/fixtures.ts`

**Acceptance Criteria:**
- [ ] `bun run check` passes with no type errors
- [ ] CardsView's statusline branch (`anthWindows`) maps every entry in `rate_limits.windows`, not just `five_hour`/`seven_day`
- [ ] A new gallery state exists showing 3 statusline-sourced windows, visible at `/gallery`

**Verify:** `bun run check` -> no errors; manual: `bun run dev`, open `http://localhost:1420/gallery`, confirm the new "Cards - statusline 3 windows" state renders 3 window rows in the Anthropic card

**Steps:**

- [ ] **Step 1: Update `CardsView.svelte`'s statusline branch**

In `src/lib/components/CardsView.svelte`, change:

```javascript
  const anthWindows = $derived.by<CardWindow[]>(() => {
    const rl = snapshot.claude_statusline?.payload.rate_limits;
    if (rl?.five_hour) {
      const out: CardWindow[] = [];
      out.push({ label: '5-hour', used: rl.five_hour.used_percent, elapsed: paceElapsed('five_hour'),
        tone: quotaTone(rl.five_hour.used_percent), resetsAt: rl.five_hour.resets_at, title: PROV.anthropicQuotaStatusline.title });
      if (rl.seven_day)
        out.push({ label: '7-day', used: rl.seven_day.used_percent, elapsed: paceElapsed('seven_day'),
          tone: quotaTone(rl.seven_day.used_percent), resetsAt: rl.seven_day.resets_at, title: PROV.anthropicQuotaStatusline.title });
      return out;
    }
    const cad = snapshot.claude_oauth?.cadences ?? [];
    return cad.map((c) => ({
      label: c.display_label,
      used: c.utilization_percent,
      elapsed: paceElapsed(c.key),
      tone: quotaTone(c.utilization_percent),
      resetsAt: c.resets_at,
      stale: anthStale,
      title: PROV.anthropicQuotaOauth.title,
    }));
  });
```

to:

```javascript
  const anthWindows = $derived.by<CardWindow[]>(() => {
    const rl = snapshot.claude_statusline?.payload.rate_limits;
    if (rl?.windows.length) {
      return rl.windows.map((w) => ({
        label: w.label,
        used: w.used_percent,
        elapsed: paceElapsed(w.key),
        tone: quotaTone(w.used_percent),
        resetsAt: w.resets_at,
        title: PROV.anthropicQuotaStatusline.title,
      }));
    }
    const cad = snapshot.claude_oauth?.cadences ?? [];
    return cad.map((c) => ({
      label: c.display_label,
      used: c.utilization_percent,
      elapsed: paceElapsed(c.key),
      tone: quotaTone(c.utilization_percent),
      resetsAt: c.resets_at,
      stale: anthStale,
      title: PROV.anthropicQuotaOauth.title,
    }));
  });
```

(The statusline branch drops the `stale` field, matching that `RateWindow` carries no per-window staleness signal of its own - same as before this change, where only the two hardcoded pushes existed and neither set `stale` either. `CardWindow.stale` is optional, so omitting it is valid.)

Also update `anthSource`, which checks the old `rl?.five_hour` shape to decide which source drives the Anthropic card - change:

```javascript
  const anthSource = $derived(snapshot.claude_statusline?.payload.rate_limits?.five_hour ? 'statusline' : 'oauth');
```

to:

```javascript
  const anthSource = $derived(snapshot.claude_statusline?.payload.rate_limits?.windows.length ? 'statusline' : 'oauth');
```

- [ ] **Step 2: Run the type check**

Run: `bun run check`
Expected: PASS (0 errors)

- [ ] **Step 3: Add a gallery fixture with 3 statusline windows**

In `src/lib/gallery/fixtures.ts`, add a new function right after `overageBilled()` (before the `GalleryState` interface):

```typescript
/** Statusline-sourced Anthropic quota with 3 windows (5h, 7d, and an
 * unrecognized 3rd window) - the states gallery previously had zero
 * coverage of the statusline branch at all, even for the 2-window case. */
function statuslineThreeWindows(): Snapshot {
  const s = clone(baseSnapshot());
  s.claude_oauth = null;
  s.claude_oauth_error = null;
  s.claude_statusline = {
    schema_version: 2,
    captured_at: iso(0),
    payload: {
      rate_limits: {
        windows: [
          { key: 'five_hour', label: '5-hour', used_percent: 17, resets_at: iso(4 * H) },
          { key: 'seven_day', label: '7-day', used_percent: 2, resets_at: iso(120 * H) },
          { key: 'seven_day_fable', label: 'Seven Day Fable', used_percent: 0, resets_at: iso(120 * H) },
        ],
      },
      session_cost_micro_usd: 4_200_000,
      claude_code_version: '2.1.144',
    },
  };
  s.claude_statusline_error = null;
  return s;
}
```

Note: the TS `StatuslineSnapshot` interface (`src/lib/types/snapshot.ts`) doesn't mirror the Rust struct's `model_display_name`/`context_used_percent` fields at all - a pre-existing gap, unrelated to this change and out of scope to fix here. The fixture above only uses fields the current TS interface actually has.

- [ ] **Step 4: Register the new gallery state**

In the same file's `GALLERY_STATES` array, add this entry right after the `'Cards - Anthropic statusline fallback'` entry:

```typescript
  {
    label: 'Cards - statusline 3 windows (Fable)',
    view: 'cards',
    openaiEnabled: true,
    snapshot: statuslineThreeWindows(),
  },
```

- [ ] **Step 5: Run the type check again**

Run: `bun run check`
Expected: PASS (0 errors)

- [ ] **Step 6: Manual visual verification**

Run: `bun run dev`
Open: `http://localhost:1420/gallery`
Expected: the "Cards - statusline 3 windows (Fable)" state shows the Anthropic card with 3 window rows (5-hour, 7-day, Seven Day Fable), each with its own progress bar and reset countdown.

- [ ] **Step 7: Commit**

```bash
git add src/lib/components/CardsView.svelte src/lib/gallery/fixtures.ts
git commit -m "feat(statusline): CardsView renders every statusline rate-limit window

Mirrors the OAuth branch's existing generic cadences.map() instead of
hardcoding 5-hour/7-day. Adds the gallery's first fixture with a
populated statusline rate_limits block (previously zero coverage, even
for the 2-window case) so this and the existing 2-window path both get
visual verification via /gallery."
```

---

### Task 7: Docs - `docs/ARCHITECTURE.md`

**Goal:** Document the new `windows` field on both the on-disk IPC file and the CLI `--json` schema, per AGENTS.md's change-control rule for schema changes.

**Files:**
- Modify: `docs/ARCHITECTURE.md`

**Acceptance Criteria:**
- [ ] The on-disk IPC files table's `statusline.snapshot.json` row mentions the schema version bump / generic windows
- [ ] The CLI `--json` schema paragraph mentions the new `windows` field

**Verify:** Manual read-through; no automated check (docs-only change)

**Steps:**

- [ ] **Step 1: Update the CLI `--json` schema paragraph**

In `docs/ARCHITECTURE.md`, find the paragraph starting "The CLI `--json` schema is the same `Snapshot` rendered through a presentation DTO..." and change this sentence:

```
Two extra cells (v0.2): `claude_statusline` carries the live `StatuslineFilePayload` envelope (Claude Code's session estimate - a distinct cost tier, no money normalization); `.pace` carries a per-window array (`key`, `used_fraction`, `elapsed_fraction`, `ratio`) derived from the OAuth cadence bars - used % vs elapsed % of each quota window (5h, 7d) plus their ratio, computed by `window::pace`; `ratio` is null right after a window reset.
```

to:

```
Two extra cells (v0.2): `claude_statusline` carries the live `StatuslineFilePayload` envelope (Claude Code's session estimate - a distinct cost tier, no money normalization); its `rate_limits` block carries `five_hour`/`seven_day` (unchanged position) plus a `windows` array with every rate-limit window the statusLine payload reported, mirroring `claude_oauth.cadences` - a statusLine-sourced snapshot is never capped at the two named windows. `.pace` carries a per-window array (`key`, `used_fraction`, `elapsed_fraction`, `ratio`) derived from the OAuth cadence bars - used % vs elapsed % of each quota window (5h, 7d) plus their ratio, computed by `window::pace`; `ratio` is null right after a window reset.
```

- [ ] **Step 2: Update the on-disk IPC files table**

In the same file, find this table row:

```
| `statusline.snapshot.json` | `balanze-cli statusline` (every turn) | `watcher::tasks::statusline` (on notify) | `StatuslineFilePayload` - Claude session estimate + rate-limit windows (boundary #12) |
```

change to:

```
| `statusline.snapshot.json` | `balanze-cli statusline` (every turn) | `watcher::tasks::statusline` (on notify) | `StatuslineFilePayload` (`schema_version` 2) - Claude session estimate + a generic rate-limit window list (`RateLimits.windows`, not just five_hour/seven_day; boundary #12) |
```

- [ ] **Step 3: Commit**

```bash
git add docs/ARCHITECTURE.md
git commit -m "docs(architecture): document the generic statusline rate-limit windows

Updates the CLI --json schema paragraph and the on-disk IPC files table
for RateLimits.windows and the SCHEMA_VERSION bump to 2."
```

---

## Final full-workspace verification (after all 7 tasks)

- [ ] `cargo build --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo nextest run --workspace`
- [ ] `cargo fmt --all -- --check`
- [ ] `bun run check`
- [ ] Manual: `bun run tauri dev` - tray icon appears, popover opens, Cards view's Anthropic card renders correctly for a real (2-window) account, no console errors
