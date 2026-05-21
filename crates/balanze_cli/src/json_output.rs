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
//!    uniformly — no provider-specific keys for the headline number. Inner
//!    rich detail (per-model breakdown, by-line-item, currency, etc.) stays
//!    under `details` so nothing is lost.
//! 2. **Tags source + confidence on every money cell.** A JSONL × list-price
//!    estimate (`jsonl_list_price` / `estimate`) cannot be confused with the
//!    OpenAI Admin Costs API figure (`openai_admin_costs` / `real`) or with
//!    the pay-as-you-go overage (`extra_usage_billed` / `real`) — the
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
use chrono::{DateTime, Duration, Utc};
use claude_cost::{Cost, ModelCost};
use claude_statusline::StatuslineFilePayload;
use codex_local::{CodexQuotaSnapshot, RateLimitWindow};
use openai_client::{LineItemCost, OpenAiCosts};
use serde::Serialize;
use state_coordinator::{JsonlSnapshot, Prediction, PredictionState, Snapshot};

/// Sentinel inserted in place of `codex_quota.session_id` when `verbose=false`.
const SESSION_ID_REDACTED: &str = "<redacted>";

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
/// snapshot is exactly one line — preserving the "one JSON object per
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
    prediction: Option<JsonPrediction>,
}

impl<'a> JsonDoc<'a> {
    fn from_snapshot(snap: &'a Snapshot, verbose: bool) -> Self {
        Self {
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
            prediction: snap.prediction.as_ref().map(JsonPrediction::from),
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
    /// `None` (serialized as `null`) when not verbose — safe to paste publicly.
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
    /// The pay-as-you-go ceiling — preserved at the top level because both
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
    /// OpenAI's wire shape is `total_usd: f64`. We convert to i64 micro-USD
    /// at the JSON boundary (AGENTS.md §2.1: integer math everywhere
    /// internally; f64 only at the display boundary — `--json` IS that
    /// boundary for scripts) so consumers can read the same
    /// `.value_micro_usd` they read on other cells.
    value_micro_usd: i64,
    source: &'static str,
    confidence: &'static str,
    details: JsonOpenAiDetails<'a>,
}

#[derive(Serialize)]
struct JsonOpenAiDetails<'a> {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    /// Preserved for provenance (OpenAI's wire field). Reads should prefer
    /// the top-level `value_micro_usd`.
    total_usd: f64,
    by_line_item: &'a [LineItemCost],
    truncated: bool,
    fetched_at: DateTime<Utc>,
}

impl<'a> From<&'a OpenAiCosts> for JsonOpenAi<'a> {
    fn from(o: &'a OpenAiCosts) -> Self {
        // Round on conversion — `*_usd * 1_000_000` then floor-as-i64 would
        // lose half a micro-USD on every cell.
        let value_micro_usd = (o.total_usd * 1_000_000.0).round() as i64;
        Self {
            value_micro_usd,
            source: "openai_admin_costs",
            confidence: "real",
            details: JsonOpenAiDetails {
                start_time: o.start_time,
                end_time: o.end_time,
                total_usd: o.total_usd,
                by_line_item: &o.by_line_item,
                truncated: o.truncated,
                fetched_at: o.fetched_at,
            },
        }
    }
}

// ----------------------------------------------------------------------------
// codex_quota (rate-limit % — not a money cell, but redaction applies)
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
// claude_statusline (statusLine file payload — session cost + rate windows)
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonClaudeStatusline {
    schema_version: u8,
    captured_at: DateTime<Utc>,
    five_hour: Option<JsonRateWindow>,
    seven_day: Option<JsonRateWindow>,
    /// Session cost converted from i64 micro-USD to f64 dollars at the JSON
    /// boundary (AGENTS.md §2.1 — f64 only at display).
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

// ----------------------------------------------------------------------------
// prediction (EWMA predictor output)
// ----------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonPrediction {
    state: &'static str,
    eta_to_cap_seconds: Option<i64>,
    eta_to_reset_seconds: i64,
    computed_at: DateTime<Utc>,
    source: &'static str,
    confidence: &'static str,
}

impl From<&Prediction> for JsonPrediction {
    fn from(p: &Prediction) -> Self {
        Self {
            state: match p.state {
                PredictionState::Insufficient => "Insufficient",
                PredictionState::Uncertain => "Uncertain",
                PredictionState::Confident => "Confident",
            },
            eta_to_cap_seconds: p.eta_to_cap.map(|d: Duration| d.num_seconds()),
            eta_to_reset_seconds: p.eta_to_reset.num_seconds(),
            computed_at: p.computed_at,
            source: "predictor_ewma",
            confidence: "estimate",
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
            total_usd: 4.20,
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
        // Provenance preserved: original f64 is still under details.
        assert!((cell["details"]["total_usd"].as_f64().unwrap() - 4.20).abs() < 1e-9);
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
        // Tokens aren't money — claude_jsonl is rendered as-is, no
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
        };
        StatuslineFilePayload::new(snap, fixed_now())
    }

    fn sample_prediction() -> Prediction {
        // Build a Confident prediction directly: warm-up passed, cap not reached.
        Prediction {
            state: state_coordinator::PredictionState::Confident,
            eta_to_cap: Some(chrono::Duration::seconds(11_280)),
            eta_to_reset: chrono::Duration::seconds(16_200),
            computed_at: fixed_now(),
        }
    }

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

    #[test]
    fn claude_statusline_absent_is_null() {
        let s = Snapshot::empty(fixed_now());
        let v = render_to_value(&s, false);
        assert!(v["claude_statusline"].is_null());
    }

    #[test]
    fn prediction_cell_shape() {
        let mut s = Snapshot::empty(fixed_now());
        s.prediction = Some(sample_prediction());
        let v = render_to_value(&s, false);
        let cell = &v["prediction"];
        // sample_prediction() constructs PredictionState::Confident — assert the
        // exact string so a state swap (e.g. Confident → Insufficient) is caught.
        assert_eq!(cell["state"].as_str().unwrap(), "Confident");
        assert_eq!(cell["source"], "predictor_ewma");
        assert_eq!(cell["confidence"], "estimate");
        // eta_to_reset: 16 200 s from sample_prediction()
        assert_eq!(cell["eta_to_reset_seconds"].as_i64().unwrap(), 16_200);
        // eta_to_cap: 11 280 s from sample_prediction() (Some → not null)
        assert_eq!(cell["eta_to_cap_seconds"].as_i64().unwrap(), 11_280);
        // computed_at serialises to an ISO-8601 string
        assert!(cell["computed_at"].is_string());
    }

    #[test]
    fn prediction_cell_insufficient_state_serializes_eta_as_null() {
        // Build an Insufficient prediction (warm-up / too few history points).
        // eta_to_cap must be None → serialises as JSON null.
        // eta_to_reset is always present.
        let insufficient = state_coordinator::Prediction {
            state: state_coordinator::PredictionState::Insufficient,
            eta_to_cap: None,
            eta_to_reset: chrono::Duration::seconds(16_200),
            computed_at: fixed_now(),
        };
        let mut s = Snapshot::empty(fixed_now());
        s.prediction = Some(insufficient);
        let v = render_to_value(&s, false);
        let cell = &v["prediction"];
        assert_eq!(cell["state"].as_str().unwrap(), "Insufficient");
        // Option<Duration> with None must serialise as JSON null.
        assert!(
            cell["eta_to_cap_seconds"].is_null(),
            "eta_to_cap_seconds should be null for Insufficient, got {}",
            cell["eta_to_cap_seconds"]
        );
        // eta_to_reset is always present regardless of state.
        assert_eq!(
            cell["eta_to_reset_seconds"].as_i64().unwrap(),
            16_200,
            "eta_to_reset_seconds must always be an i64"
        );
    }

    #[test]
    fn prediction_absent_is_null() {
        let s = Snapshot::empty(fixed_now());
        let v = render_to_value(&s, false);
        assert!(v["prediction"].is_null());
    }
}
