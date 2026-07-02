//! Public JSON output for `balanze-cli status --json`.
//!
//! The in-memory [`state_coordinator::Snapshot`] uses concrete provider types
//! (`claude_cost::Cost`, `openai_client::OpenAiCosts`, `anthropic_oauth::ExtraUsage`)
//! whose field names differ per provider. That's fine for in-process IPC and
//! the Tauri-side glue, but a script consuming `--json` cannot tell estimate
//! vs real spend just by looking at field names.
//!
//! This module renders a thin presentation DTO that:
//!
//! 1. **Normalizes every money cell** to `{ value_micro_usd, source,
//!    confidence, details }`. A consumer reads `.anthropic_api_cost.value_micro_usd`,
//!    `.openai.value_micro_usd`, and `.claude_oauth.extra_usage.value_micro_usd`
//!    uniformly - no provider-specific keys for the headline number. Inner
//!    rich detail (per-model breakdown, by-line-item, currency, etc.) stays
//!    under `details` so nothing is lost.
//! 2. **Tags source + confidence on every money cell.** A JSONL × list-price
//!    estimate (`jsonl_list_price` / `estimate`) cannot be confused with the
//!    OpenAI Admin Costs API figure (`openai_admin_costs` / `real`) or with
//!    the pay-as-you-go overage (`extra_usage_billed` / `real`) - the
//!    distinction is explicit in the wire shape, not buried in a label.
//! 3. **Redacts account-identifying fields by default.** `claude_oauth.org_uuid`
//!    and `codex_quota.session_id` are nullified / replaced with `<redacted>`
//!    unless `verbose=true` (`-v` from the CLI). Safe to paste into a bug
//!    report without doxing the user.
//!
//! The shape is documented in `AGENTS.md` §2.1 (the "CLI `--json` schema" row
//! of the project-conventions table). Schema changes require updating that
//! row, this module's tests, and the README example block in lockstep.

use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot, ExtraUsage};
use chrono::{DateTime, Utc};
use claude_cost::{Cost, ModelCost};
use claude_statusline::StatuslineFilePayload;
use codex_local::{CodexQuotaSnapshot, RateLimitWindow};
use openai_client::{LineItemCost, OpenAiCosts};
use serde::Serialize;
use state_coordinator::{JsonlSnapshot, Snapshot, WindowPace};

/// Sentinel inserted in place of `codex_quota.session_id` when `verbose=false`.
const SESSION_ID_REDACTED: &str = "<redacted>";

/// Schema version of the `--json` presentation DTO (this module's shape). Bump
/// when the wire shape changes so machine consumers can detect a break; keep the
/// AGENTS.md §2.1 `--json` row and the README example in lockstep. Independent
/// of `state_coordinator::SNAPSHOT_SCHEMA_VERSION` (the IPC `Snapshot` is a
/// different surface). Version 1 is the first explicitly-versioned schema and
/// carries the i64-micro-USD OpenAI cell shape.
const SCHEMA_VERSION: u32 = 1;

/// Serialize `snap` as pretty-printed JSON suitable for `balanze-cli status
/// --json`. When `verbose=false`, account identifiers (`org_uuid`,
/// `session_id`) are redacted.
pub fn render(snap: &Snapshot, verbose: bool) -> Result<String, serde_json::Error> {
    let doc = JsonDoc::from_snapshot(snap, verbose);
    serde_json::to_string_pretty(&doc)
}

/// Serialize `snap` as a single-line JSON document suitable for JSONL
/// streams (e.g. `balanze-cli --watch --json | jq`). Same data shape +
/// redaction rules as [`render`], but no embedded newlines so each
/// snapshot is exactly one line - preserving the "one JSON object per
/// line" invariant that line-oriented consumers depend on.
pub fn render_jsonl(snap: &Snapshot, verbose: bool) -> Result<String, serde_json::Error> {
    let doc = JsonDoc::from_snapshot(snap, verbose);
    serde_json::to_string(&doc)
}

// ----------------------------------------------------------------------------
// Top-level document.
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonDoc<'a> {
    schema_version: u32,
    fetched_at: DateTime<Utc>,
    claude_oauth: Option<JsonClaudeOAuth<'a>>,
    claude_oauth_error: Option<&'a str>,
    claude_jsonl: Option<&'a JsonlSnapshot>,
    claude_jsonl_error: Option<&'a str>,
    anthropic_api_cost: Option<JsonAnthropicApiCost<'a>>,
    anthropic_api_cost_error: Option<&'a str>,
    codex_quota: Option<JsonCodexQuota<'a>>,
    codex_quota_error: Option<&'a str>,
    openai: Option<JsonOpenAi<'a>>,
    openai_error: Option<&'a str>,
    claude_statusline: Option<JsonClaudeStatusline>,
    claude_statusline_error: Option<&'a str>,
    pace: Vec<JsonPace>,
}

impl<'a> JsonDoc<'a> {
    fn from_snapshot(snap: &'a Snapshot, verbose: bool) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            fetched_at: snap.fetched_at,
            claude_oauth: snap
                .claude_oauth
                .as_ref()
                .map(|o| JsonClaudeOAuth::from_snapshot(o, verbose)),
            claude_oauth_error: snap.claude_oauth_error.as_deref(),
            claude_jsonl: snap.claude_jsonl.as_ref(),
            claude_jsonl_error: snap.claude_jsonl_error.as_deref(),
            anthropic_api_cost: snap
                .anthropic_api_cost
                .as_ref()
                .map(JsonAnthropicApiCost::from),
            anthropic_api_cost_error: snap.anthropic_api_cost_error.as_deref(),
            codex_quota: snap
                .codex_quota
                .as_ref()
                .map(|c| JsonCodexQuota::from_snapshot(c, verbose)),
            codex_quota_error: snap.codex_quota_error.as_deref(),
            openai: snap.openai.as_ref().map(JsonOpenAi::from),
            openai_error: snap.openai_error.as_deref(),
            claude_statusline: snap
                .claude_statusline
                .as_ref()
                .map(JsonClaudeStatusline::from),
            claude_statusline_error: snap.claude_statusline_error.as_deref(),
            pace: snap.pace.iter().map(JsonPace::from).collect(),
        }
    }
}

// ----------------------------------------------------------------------------
// claude_oauth (cadence bars + extra_usage + identifying fields)
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonClaudeOAuth<'a> {
    cadences: &'a [CadenceBar],
    extra_usage: Option<JsonExtraUsage<'a>>,
    subscription_type: Option<&'a str>,
    rate_limit_tier: Option<&'a str>,
    /// Identifies the user's Anthropic consumer subscription org. Set to
    /// `None` (serialized as `null`) when not verbose - safe to paste publicly.
    org_uuid: Option<&'a str>,
    fetched_at: DateTime<Utc>,
}

impl<'a> JsonClaudeOAuth<'a> {
    fn from_snapshot(o: &'a ClaudeOAuthSnapshot, verbose: bool) -> Self {
        Self {
            cadences: &o.cadences,
            extra_usage: o.extra_usage.as_ref().map(JsonExtraUsage::from),
            subscription_type: o.subscription_type.as_deref(),
            rate_limit_tier: o.rate_limit_tier.as_deref(),
            org_uuid: if verbose { o.org_uuid.as_deref() } else { None },
            fetched_at: o.fetched_at,
        }
    }
}

#[derive(Serialize)]
struct JsonExtraUsage<'a> {
    /// Headline number for downstream "how much is the user spending" reads:
    /// the spent amount (`used_credits`), in i64 micro-USD per AGENTS.md §2.1.
    value_micro_usd: i64,
    /// The pay-as-you-go ceiling - preserved at the top level because both
    /// the limit and the spent figure are billed-real numbers a consumer
    /// might want without diving into details.
    monthly_limit_micro_usd: i64,
    source: &'static str,
    confidence: &'static str,
    details: JsonExtraUsageDetails<'a>,
}

#[derive(Serialize)]
struct JsonExtraUsageDetails<'a> {
    is_enabled: bool,
    utilization_percent: f32,
    currency: &'a str,
}

impl<'a> From<&'a ExtraUsage> for JsonExtraUsage<'a> {
    fn from(eu: &'a ExtraUsage) -> Self {
        Self {
            value_micro_usd: eu.used_credits_micro_usd,
            monthly_limit_micro_usd: eu.monthly_limit_micro_usd,
            source: "extra_usage_billed",
            confidence: "real",
            details: JsonExtraUsageDetails {
                is_enabled: eu.is_enabled,
                utilization_percent: eu.utilization_percent,
                currency: &eu.currency,
            },
        }
    }
}

// ----------------------------------------------------------------------------
// anthropic_api_cost (JSONL × LiteLLM estimate)
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonAnthropicApiCost<'a> {
    value_micro_usd: i64,
    source: &'static str,
    confidence: &'static str,
    details: JsonCostDetails<'a>,
}

#[derive(Serialize)]
struct JsonCostDetails<'a> {
    per_model: &'a [ModelCost],
    skipped_models: &'a [String],
    total_event_count: usize,
    unparsed_event_count: usize,
}

impl<'a> From<&'a Cost> for JsonAnthropicApiCost<'a> {
    fn from(c: &'a Cost) -> Self {
        Self {
            value_micro_usd: c.total_micro_usd,
            source: "jsonl_list_price",
            confidence: "estimate",
            details: JsonCostDetails {
                per_model: &c.per_model,
                skipped_models: &c.skipped_models,
                total_event_count: c.total_event_count,
                unparsed_event_count: c.unparsed_event_count,
            },
        }
    }
}

// ----------------------------------------------------------------------------
// openai (Admin Costs API real spend)
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonOpenAi<'a> {
    /// Headline OpenAI spend in i64 micro-USD (AGENTS.md §2.1). `OpenAiCosts`
    /// is already micro-USD - converted at the parse boundary in
    /// `openai_client` - so this is a straight read, uniform with the other
    /// money cells' `value_micro_usd`.
    value_micro_usd: i64,
    source: &'static str,
    confidence: &'static str,
    details: JsonOpenAiDetails<'a>,
}

#[derive(Serialize)]
struct JsonOpenAiDetails<'a> {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    /// Per-line-item breakdown; each `amount_micro_usd` is i64 micro-USD.
    by_line_item: &'a [LineItemCost],
    truncated: bool,
    fetched_at: DateTime<Utc>,
}

impl<'a> From<&'a OpenAiCosts> for JsonOpenAi<'a> {
    fn from(o: &'a OpenAiCosts) -> Self {
        // `OpenAiCosts` is already i64 micro-USD (converted at the parse
        // boundary), so the headline value is a straight read - no f64 here.
        Self {
            value_micro_usd: o.total_micro_usd,
            source: "openai_admin_costs",
            confidence: "real",
            details: JsonOpenAiDetails {
                start_time: o.start_time,
                end_time: o.end_time,
                by_line_item: &o.by_line_item,
                truncated: o.truncated,
                fetched_at: o.fetched_at,
            },
        }
    }
}

// ----------------------------------------------------------------------------
// codex_quota (rate-limit % - not a money cell, but redaction applies)
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonCodexQuota<'a> {
    observed_at: DateTime<Utc>,
    /// Replaced with the literal string `"<redacted>"` when not verbose.
    /// A `String` (never `Option<String>`) in the source struct, so we
    /// keep the field present and substitute the value rather than
    /// nullifying.
    session_id: &'a str,
    primary: &'a RateLimitWindow,
    secondary: Option<&'a RateLimitWindow>,
    plan_type: &'a str,
    rate_limit_reached: bool,
}

impl<'a> JsonCodexQuota<'a> {
    fn from_snapshot(c: &'a CodexQuotaSnapshot, verbose: bool) -> Self {
        Self {
            observed_at: c.observed_at,
            session_id: if verbose {
                &c.session_id
            } else {
                SESSION_ID_REDACTED
            },
            primary: &c.primary,
            secondary: c.secondary.as_ref(),
            plan_type: &c.plan_type,
            rate_limit_reached: c.rate_limit_reached,
        }
    }
}

// ----------------------------------------------------------------------------
// claude_statusline (statusLine file payload - session cost + rate windows)
// ----------------------------------------------------------------------------

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

// ----------------------------------------------------------------------------
// pace (used vs elapsed, per cadence window)
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonPace {
    key: String,
    used_fraction: f64,
    elapsed_fraction: f64,
    ratio: Option<f64>,
}

impl From<&WindowPace> for JsonPace {
    fn from(p: &WindowPace) -> Self {
        Self {
            key: p.key.clone(),
            used_fraction: p.used_fraction,
            elapsed_fraction: p.elapsed_fraction,
            ratio: p.ratio,
        }
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use anthropic_oauth::ClaudeOAuthSnapshot;
    use chrono::TimeZone;
    use claude_cost::{Cost, ModelCost};
    use claude_statusline::{StatuslineFilePayload, StatuslineSnapshot};
    use codex_local::{CodexQuotaSnapshot, RateLimitWindow};
    use openai_client::OpenAiCosts;
    use serde_json::Value;
    use state_coordinator::{JsonlSnapshot, Snapshot};
    use window::WindowSummary;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap()
    }

    fn sample_cost() -> Cost {
        Cost {
            per_model: vec![ModelCost {
                model: "claude-sonnet-4-6".to_string(),
                event_count: 1,
                input_micro_usd: 3_000,
                output_micro_usd: 7_500,
                cache_creation_micro_usd: 0,
                cache_read_micro_usd: 0,
                total_micro_usd: 10_500,
            }],
            total_micro_usd: 10_500,
            skipped_models: vec![],
            total_event_count: 1,
            unparsed_event_count: 0,
        }
    }

    fn sample_openai() -> OpenAiCosts {
        OpenAiCosts {
            start_time: fixed_now() - chrono::Duration::days(20),
            end_time: fixed_now(),
            total_micro_usd: 4_200_000,
            by_line_item: vec![],
            truncated: false,
            fetched_at: fixed_now(),
        }
    }

    fn sample_oauth(with_extra_usage: bool, with_org_uuid: bool) -> ClaudeOAuthSnapshot {
        ClaudeOAuthSnapshot {
            cadences: vec![],
            extra_usage: if with_extra_usage {
                Some(anthropic_oauth::ExtraUsage {
                    is_enabled: true,
                    monthly_limit_micro_usd: 25_000_000,
                    used_credits_micro_usd: 20_920_000,
                    utilization_percent: 83.7,
                    currency: "USD".to_string(),
                })
            } else {
                None
            },
            subscription_type: Some("max".to_string()),
            rate_limit_tier: Some("default_claude_max_5x".to_string()),
            org_uuid: if with_org_uuid {
                Some("aaaa-bbbb-cccc-dddd".to_string())
            } else {
                None
            },
            fetched_at: fixed_now(),
        }
    }

    fn sample_codex() -> CodexQuotaSnapshot {
        CodexQuotaSnapshot {
            observed_at: fixed_now(),
            session_id: "11111111-2222-3333-4444-555555555555".to_string(),
            primary: RateLimitWindow {
                used_percent: 6.0,
                window_duration_minutes: 10_080,
                resets_at: fixed_now() + chrono::Duration::days(7),
            },
            secondary: None,
            plan_type: "go".to_string(),
            rate_limit_reached: false,
        }
    }

    fn populated_snapshot() -> Snapshot {
        let mut s = Snapshot::empty(fixed_now());
        s.claude_oauth = Some(sample_oauth(true, true));
        s.claude_jsonl = Some(JsonlSnapshot {
            files_scanned: 3,
            window: WindowSummary {
                window_start: fixed_now() - chrono::Duration::hours(5),
                total_events_in_window: 1,
                total_tokens_in_window: 1500,
                recent_burn_tokens_per_min: None,
                by_model: vec![],
            },
        });
        s.anthropic_api_cost = Some(sample_cost());
        s.codex_quota = Some(sample_codex());
        s.openai = Some(sample_openai());
        s
    }

    fn render_to_value(snap: &Snapshot, verbose: bool) -> Value {
        let json = render(snap, verbose).expect("render");
        serde_json::from_str(&json).expect("valid json")
    }

    #[test]
    fn empty_snapshot_serializes_with_null_money_cells() {
        let s = Snapshot::empty(fixed_now());
        let v = render_to_value(&s, false);
        // The DTO carries an explicit, top-level schema version for consumers.
        assert_eq!(v["schema_version"], 1);
        assert!(v["anthropic_api_cost"].is_null());
        assert!(v["openai"].is_null());
        assert!(v["claude_oauth"].is_null());
        assert!(v["codex_quota"].is_null());
    }

    #[test]
    fn anthropic_api_cost_is_tagged_as_jsonl_list_price_estimate() {
        let v = render_to_value(&populated_snapshot(), false);
        let cell = &v["anthropic_api_cost"];
        assert_eq!(cell["value_micro_usd"], 10_500);
        assert_eq!(cell["source"], "jsonl_list_price");
        assert_eq!(cell["confidence"], "estimate");
        // Inner rich detail preserved under `details`.
        assert_eq!(cell["details"]["total_event_count"], 1);
        assert!(cell["details"]["per_model"].is_array());
        assert_eq!(
            cell["details"]["per_model"][0]["model"],
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn openai_is_tagged_as_openai_admin_costs_real_with_micro_usd_normalization() {
        let v = render_to_value(&populated_snapshot(), false);
        let cell = &v["openai"];
        // $4.20 → 4_200_000 micro-USD after rounding.
        assert_eq!(cell["value_micro_usd"], 4_200_000);
        assert_eq!(cell["source"], "openai_admin_costs");
        assert_eq!(cell["confidence"], "real");
        // OpenAiCosts is i64 micro-USD now, so the old f64 `details.total_usd`
        // provenance field is gone; the per-line-item breakdown remains.
        assert!(cell["details"]["total_usd"].is_null());
        assert!(cell["details"]["by_line_item"].is_array());
    }

    #[test]
    fn extra_usage_is_tagged_as_extra_usage_billed_real() {
        let v = render_to_value(&populated_snapshot(), false);
        let eu = &v["claude_oauth"]["extra_usage"];
        assert_eq!(eu["value_micro_usd"], 20_920_000);
        assert_eq!(eu["monthly_limit_micro_usd"], 25_000_000);
        assert_eq!(eu["source"], "extra_usage_billed");
        assert_eq!(eu["confidence"], "real");
        assert_eq!(eu["details"]["currency"], "USD");
        assert!(eu["details"]["is_enabled"].as_bool().unwrap());
    }

    #[test]
    fn over_limit_extra_usage_still_surfaces_real_billed_value() {
        // Once usage exceeds the cap, Anthropic flips is_enabled=false but keeps
        // the real billed numbers. --json must still surface that value (a
        // consumer reads value_micro_usd + is_enabled), never drop it.
        let mut s = populated_snapshot();
        s.claude_oauth.as_mut().unwrap().extra_usage = Some(anthropic_oauth::ExtraUsage {
            is_enabled: false,
            monthly_limit_micro_usd: 45_000_000,
            used_credits_micro_usd: 45_580_000,
            utilization_percent: 100.0,
            currency: "USD".to_string(),
        });
        let v = render_to_value(&s, false);
        let eu = &v["claude_oauth"]["extra_usage"];
        assert_eq!(eu["value_micro_usd"], 45_580_000);
        assert_eq!(eu["monthly_limit_micro_usd"], 45_000_000);
        assert_eq!(eu["source"], "extra_usage_billed");
        assert!(!eu["details"]["is_enabled"].as_bool().unwrap());
    }

    #[test]
    fn extra_usage_absent_is_null_not_present_as_empty_object() {
        let mut s = populated_snapshot();
        s.claude_oauth = Some(sample_oauth(false, true));
        let v = render_to_value(&s, false);
        assert!(v["claude_oauth"]["extra_usage"].is_null());
    }

    #[test]
    fn non_verbose_redacts_org_uuid_to_null() {
        let v = render_to_value(&populated_snapshot(), false);
        assert!(v["claude_oauth"]["org_uuid"].is_null());
    }

    #[test]
    fn non_verbose_redacts_codex_session_id_to_sentinel_string() {
        let v = render_to_value(&populated_snapshot(), false);
        assert_eq!(v["codex_quota"]["session_id"], "<redacted>");
    }

    #[test]
    fn verbose_reveals_org_uuid_and_session_id() {
        let v = render_to_value(&populated_snapshot(), true);
        assert_eq!(v["claude_oauth"]["org_uuid"], "aaaa-bbbb-cccc-dddd");
        assert_eq!(
            v["codex_quota"]["session_id"],
            "11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn render_jsonl_emits_exactly_one_line() {
        // The JSONL contract: each Snapshot serialized via render_jsonl must
        // contain no embedded newlines so that line-oriented consumers
        // (`jq`, log-aggregators, `--watch --json | head`) see exactly one
        // JSON object per line. `render` uses to_string_pretty (multi-line)
        // and is for the one-shot `--json` view; the JSONL variant must
        // not.
        let snap = populated_snapshot();
        let line = render_jsonl(&snap, false).expect("render_jsonl ok");
        assert!(
            !line.contains('\n'),
            "render_jsonl output must not contain embedded newlines (would break JSONL streams); got:\n{line}"
        );
        // Still parseable as the same shape.
        let v: Value = serde_json::from_str(&line).expect("compact JSON parses");
        assert!(v["anthropic_api_cost"]["value_micro_usd"].is_i64());
    }

    #[test]
    fn money_cells_share_uniform_value_micro_usd_key() {
        // The headline contract: a consumer can read `.value_micro_usd` on
        // every money-bearing cell without knowing the source.
        let v = render_to_value(&populated_snapshot(), false);
        assert!(v["anthropic_api_cost"]["value_micro_usd"].is_i64());
        assert!(v["openai"]["value_micro_usd"].is_i64());
        assert!(v["claude_oauth"]["extra_usage"]["value_micro_usd"].is_i64());
    }

    #[test]
    fn error_strings_pass_through_unchanged() {
        let mut s = Snapshot::empty(fixed_now());
        s.openai_error = Some("HTTP 401".to_string());
        s.claude_jsonl_error = Some("permission denied".to_string());
        let v = render_to_value(&s, false);
        assert_eq!(v["openai_error"], "HTTP 401");
        assert_eq!(v["claude_jsonl_error"], "permission denied");
    }

    #[test]
    fn claude_jsonl_passes_through_without_tagging() {
        // Tokens aren't money - claude_jsonl is rendered as-is, no
        // value_micro_usd injection.
        let v = render_to_value(&populated_snapshot(), false);
        assert_eq!(v["claude_jsonl"]["files_scanned"], 3);
        assert_eq!(v["claude_jsonl"]["total_tokens_in_window"], 1500);
        assert!(v["claude_jsonl"]["value_micro_usd"].is_null()); // explicitly NOT injected
    }

    fn sample_statusline_payload() -> StatuslineFilePayload {
        let snap = StatuslineSnapshot {
            rate_limits: None,
            session_cost_micro_usd: Some(3_420_000),
            claude_code_version: Some("v2.1.144".to_string()),
            model_display_name: None,
            context_used_percent: None,
        };
        StatuslineFilePayload::new(snap, fixed_now())
    }

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
        assert_eq!(
            windows.len(),
            3,
            "all 3 windows present, not just the 2 named ones"
        );
        let keys: Vec<&str> = windows.iter().map(|w| w["key"].as_str().unwrap()).collect();
        assert_eq!(keys, vec!["five_hour", "seven_day", "seven_day_fable"]);
        let fable = windows
            .iter()
            .find(|w| w["key"] == "seven_day_fable")
            .expect("unknown-key window present");
        assert_eq!(fable["label"], "Seven Day Fable");
    }

    #[test]
    fn claude_statusline_absent_is_null() {
        let s = Snapshot::empty(fixed_now());
        let v = render_to_value(&s, false);
        assert!(v["claude_statusline"].is_null());
    }

    // --- pace cell shape ---

    fn make_pace_entries() -> Vec<state_coordinator::WindowPace> {
        vec![
            state_coordinator::WindowPace {
                key: "five_hour".to_string(),
                used_fraction: 0.82,
                elapsed_fraction: 0.40,
                ratio: Some(2.05),
            },
            state_coordinator::WindowPace {
                key: "seven_day".to_string(),
                used_fraction: 0.25,
                elapsed_fraction: 0.50,
                ratio: None,
            },
        ]
    }

    #[test]
    fn pace_cell_serializes_with_correct_keys_and_no_source() {
        let mut s = Snapshot::empty(fixed_now());
        s.pace = make_pace_entries();
        let v = render_to_value(&s, false);

        let arr = v["pace"].as_array().expect(".pace must be an array");
        assert_eq!(arr.len(), 2, ".pace must have 2 entries");

        // Find the five_hour entry and check it has exactly the documented keys.
        let five = arr
            .iter()
            .find(|e| e["key"] == "five_hour")
            .expect("five_hour entry must be present");

        assert_eq!(five["used_fraction"].as_f64().unwrap(), 0.82);
        assert_eq!(five["elapsed_fraction"].as_f64().unwrap(), 0.40);
        assert!(
            (five["ratio"].as_f64().unwrap() - 2.05).abs() < 1e-9,
            "ratio should be ~2.05, got {:?}",
            five["ratio"]
        );
        // The undocumented `source` field must NOT appear.
        assert!(
            five.get("source").is_none() || five["source"].is_null(),
            "pace entries must NOT serialize a `source` field; got {:?}",
            five.get("source")
        );
    }

    #[test]
    fn pace_entry_with_none_ratio_serializes_ratio_as_null() {
        let mut s = Snapshot::empty(fixed_now());
        s.pace = make_pace_entries();
        let v = render_to_value(&s, false);

        let arr = v["pace"].as_array().expect(".pace must be an array");
        let seven = arr
            .iter()
            .find(|e| e["key"] == "seven_day")
            .expect("seven_day entry must be present");

        // `ratio: None` → JSON `null` (field is present, value is null).
        assert!(
            seven["ratio"].is_null(),
            "ratio: None must serialize as JSON null, got {:?}",
            seven["ratio"]
        );
    }

    #[test]
    fn pace_empty_vec_serializes_pace_as_empty_array() {
        let mut s = Snapshot::empty(fixed_now());
        s.pace = Vec::new();
        let v = render_to_value(&s, false);

        let arr = v["pace"]
            .as_array()
            .expect(".pace must be a JSON array even when empty");
        assert!(arr.is_empty(), ".pace must be [] when snap.pace is empty");
    }
}
