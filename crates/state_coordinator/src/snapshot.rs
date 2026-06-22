//! The in-memory `Snapshot` and its pure merge functions.
//!
//! `Snapshot` is the canonical "everything Balanze currently knows" shape.
//! Both `balanze_cli` (single-shot) and the future `src-tauri` (long-running
//! daemon, via the coordinator actor) produce values of this type. Per
//! AGENTS.md §4 #8, identical inputs must yield identical `Snapshot`s.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, Duration, Utc};
use claude_cost::Cost;
use claude_statusline::StatuslineFilePayload;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use serde::{Deserialize, Serialize};
use window::{DEFAULT_WINDOW, Pace, SEVEN_DAY_WINDOW, WindowSummary, pace};

use crate::messages::Source;

/// Per-window pace, mirrored from the OAuth cadence bars. Replaces the retired
/// forward predictor: measured used % vs elapsed % of the window, plus their
/// ratio. One entry per cadence whose window length is known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowPace {
    /// Cadence key, e.g. `"five_hour"` / `"seven_day"`.
    pub key: String,
    pub used_fraction: f64,
    pub elapsed_fraction: f64,
    pub ratio: Option<f64>,
}

fn window_len_for(key: &str) -> Option<Duration> {
    // Cadence keys come in a bare form (`five_hour`, `seven_day`) AND
    // model-specific variants (`seven_day_sonnet`, `seven_day_opus`,
    // `seven_day_oauth_apps`, …). Match by family prefix so both forms map to
    // their window — otherwise real Max-account 7-day windows are silently
    // dropped from the pace view.
    if key.starts_with("five_hour") {
        Some(DEFAULT_WINDOW)
    } else if key.starts_with("seven_day") {
        Some(SEVEN_DAY_WINDOW)
    } else {
        None
    }
}

/// Map the OAuth cadence bars into per-window pace. Shared by `compose()` (CLI)
/// and the coordinator (watcher) so the two paths cannot diverge.
pub fn pace_for_oauth(oauth: &ClaudeOAuthSnapshot, now: DateTime<Utc>) -> Vec<WindowPace> {
    oauth
        .cadences
        .iter()
        .filter_map(|c| {
            let len = window_len_for(&c.key)?;
            let p: Pace = pace(c.utilization_percent as f64, c.resets_at, len, now);
            Some(WindowPace {
                key: c.key.clone(),
                used_fraction: p.used_fraction,
                elapsed_fraction: p.elapsed_fraction,
                ratio: p.ratio,
            })
        })
        .collect()
}

/// Canonical Balanze state. `None` fields = "not yet observed"; `*_error`
/// fields hold the most recent failure for that source. Successful data and
/// an error can coexist: the data stays (stale) while the error explains
/// why it isn't fresh.
///
/// The shape maps onto the 4-quadrant matrix (see `Source` for the
/// per-cell mapping):
///
/// - Anthropic quota %      ← `claude_oauth`
/// - Anthropic API $ (est.) ← `anthropic_api_cost`  (derived from JSONL)
/// - OpenAI Codex %         ← `codex_quota`
/// - OpenAI API $           ← `openai`
///
/// Plus `claude_jsonl` which holds the raw JSONL window math that feeds
/// both Anthropic cells, and `claude_statusline` from the statusLine file
/// payload.
// PartialEq is intentionally NOT derived: `ClaudeOAuthSnapshot` doesn't
// implement it, and the upstream change to add it would force a float-equality
// debate that doesn't pay off. Tests compare individual fields.
/// Schema version of the in-memory + `get_snapshot` IPC `Snapshot` shape.
/// Bumped when the payload changes in a way a consumer must notice. Mirrors the
/// `settings` crate's `version` discipline. The `--json` presentation DTO
/// carries its own version (see `balanze_cli::json_output`). Durable
/// `UsageEvent` / history versioning is intentionally deferred to the SQLite
/// persistence work, where the on-disk format is actually designed.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 2;

fn default_snapshot_schema_version() -> u32 {
    SNAPSHOT_SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Schema version of this payload (`SNAPSHOT_SCHEMA_VERSION`). serde-default
    /// so an older or partial document still deserializes.
    #[serde(default = "default_snapshot_schema_version")]
    pub schema_version: u32,
    /// Wall-clock time of the most recent coordinator-side merge. Updated on
    /// every successful `Update` message.
    pub fetched_at: DateTime<Utc>,
    /// Most recent successful Anthropic OAuth usage fetch. `None` until the
    /// first success.
    pub claude_oauth: Option<ClaudeOAuthSnapshot>,
    /// Most recent failure from the Anthropic OAuth poller. Coexists with
    /// `claude_oauth` when a previously-good fetch is now stale.
    pub claude_oauth_error: Option<String>,
    /// Set when Claude OAuth is intentionally unavailable / not configured -
    /// Claude Code isn't installed, so there's no credential to read. A NEUTRAL
    /// "not configured" marker, distinct from `claude_oauth_error` (a failed
    /// fetch). Mutually exclusive with `claude_oauth` data: a successful fetch
    /// clears it. serde-default so an older or partial document still
    /// deserializes.
    #[serde(default)]
    pub claude_oauth_unavailable: Option<String>,
    /// Most recent JSONL parse + window summary.
    pub claude_jsonl: Option<JsonlSnapshot>,
    /// Most recent JSONL parse failure (filesystem error, schema drift).
    pub claude_jsonl_error: Option<String>,
    /// Most recent successful Anthropic API-cost synthesis. Derived from the
    /// same JSONL slice as `claude_jsonl` via `claude_cost::compute_cost`;
    /// see that crate's docs for the "API-rate equivalent" vs "actual spend"
    /// framing. None until the first success.
    pub anthropic_api_cost: Option<Cost>,
    /// Most recent claude_cost failure. Usually means the bundled price
    /// table is malformed (shouldn't happen on a release build); occasionally
    /// surfaces if a new model name has prices the table doesn't know yet.
    pub anthropic_api_cost_error: Option<String>,
    /// Most recent successful Codex CLI rate-limit snapshot from
    /// `codex_local::read_codex_quota`. None means Codex isn't installed
    /// or no sessions have been recorded yet.
    pub codex_quota: Option<CodexQuotaSnapshot>,
    /// Most recent codex_local failure. `None` for "Codex not installed" — the
    /// `codex_quota` slot just stays None; we only set this when Codex IS
    /// installed but reading failed (permission denied, schema drift, etc.).
    pub codex_quota_error: Option<String>,
    /// Most recent successful OpenAI Admin Costs fetch.
    pub openai: Option<OpenAiCosts>,
    /// `None` means: OpenAI not configured (no key). Some(err) means
    /// configured but the fetch failed.
    pub openai_error: Option<String>,
    /// Most recent successful Claude Code statusLine file payload. `None`
    /// until the first successful read (populated by the live watcher).
    pub claude_statusline: Option<StatuslineFilePayload>,
    /// Most recent failure from the statusline reader (file missing, schema
    /// drift). Coexists with `claude_statusline` when a previously-good read
    /// is now stale.
    pub claude_statusline_error: Option<String>,
    /// Per-window pace (used vs elapsed) derived from the OAuth cadence bars.
    /// Empty until an OAuth snapshot with a known cadence is present.
    pub pace: Vec<WindowPace>,
}

impl Snapshot {
    /// An empty snapshot stamped with `fetched_at = now`. Used at coordinator
    /// startup before any source has reported in.
    pub fn empty(now: DateTime<Utc>) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            fetched_at: now,
            claude_oauth: None,
            claude_oauth_error: None,
            claude_oauth_unavailable: None,
            claude_jsonl: None,
            claude_jsonl_error: None,
            anthropic_api_cost: None,
            anthropic_api_cost_error: None,
            codex_quota: None,
            codex_quota_error: None,
            openai: None,
            openai_error: None,
            claude_statusline: None,
            claude_statusline_error: None,
            pace: Vec::new(),
        }
    }
}

/// JSONL section of the snapshot. `files_scanned` is an I/O concept tracked
/// by the producer (CLI or watcher); `window` is the pure-function output
/// from `window::summarize_window`. `#[serde(flatten)]` keeps the wire shape
/// flat for compatibility with the CLI's existing --json output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonlSnapshot {
    /// Number of `.jsonl` files the producer (CLI today; watcher later)
    /// touched on this update. Distinct from events scanned.
    pub files_scanned: usize,
    /// Pure window math from [`window::summarize_window`]. Flattened in the
    /// wire JSON so the `--json` shape stays unchanged from pre-refactor.
    #[serde(flatten)]
    pub window: WindowSummary,
}

/// Record an error for a source WITHOUT clearing its existing data. The UI
/// should render stale-but-known data with a "degraded" indicator rather
/// than blanking it.
pub fn record_error(snapshot: &mut Snapshot, source: Source, error: &str) {
    let slot = match source {
        Source::ClaudeOAuth => &mut snapshot.claude_oauth_error,
        Source::ClaudeJsonl => &mut snapshot.claude_jsonl_error,
        Source::AnthropicApiCost => &mut snapshot.anthropic_api_cost_error,
        Source::CodexQuota => &mut snapshot.codex_quota_error,
        Source::OpenAiCosts => &mut snapshot.openai_error,
        Source::ClaudeStatusline => &mut snapshot.claude_statusline_error,
    };
    *slot = Some(error.to_string());
    // A failed fetch means the source was reachable enough to be tried, so a
    // stale "not configured" marker is now wrong. Clearing it keeps the neutral
    // marker and an error from coexisting (mutually exclusive states). Only
    // ClaudeOAuth carries the marker today.
    if source == Source::ClaudeOAuth {
        snapshot.claude_oauth_unavailable = None;
    }
}

/// Mark Claude OAuth as intentionally unavailable / not configured (Claude Code
/// not detected) - a NEUTRAL state, distinct from `record_error`'s failed-fetch.
/// Clears any stale data and error for the source so its quota cell reads "not
/// configured" rather than a perpetual loading state. A later successful OAuth
/// `Update` clears the marker (see the coordinator's `apply_partial`).
pub fn record_oauth_unavailable(snapshot: &mut Snapshot, reason: &str) {
    snapshot.claude_oauth = None;
    snapshot.claude_oauth_error = None;
    snapshot.claude_oauth_unavailable = Some(reason.to_string());
}

/// Reset a source's value AND error to `None` ("not observed / not configured").
/// Used when a provider is disabled via settings so its cell stops showing
/// stale data, rather than lingering at its last-polled value.
pub fn clear_source(snapshot: &mut Snapshot, source: Source) {
    match source {
        Source::ClaudeOAuth => {
            snapshot.claude_oauth = None;
            snapshot.claude_oauth_error = None;
            snapshot.claude_oauth_unavailable = None;
        }
        Source::ClaudeJsonl => {
            snapshot.claude_jsonl = None;
            snapshot.claude_jsonl_error = None;
        }
        Source::AnthropicApiCost => {
            snapshot.anthropic_api_cost = None;
            snapshot.anthropic_api_cost_error = None;
        }
        Source::CodexQuota => {
            snapshot.codex_quota = None;
            snapshot.codex_quota_error = None;
        }
        Source::OpenAiCosts => {
            snapshot.openai = None;
            snapshot.openai_error = None;
        }
        Source::ClaudeStatusline => {
            snapshot.claude_statusline = None;
            snapshot.claude_statusline_error = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture_now, oauth_snapshot};
    use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot};

    // The "successful update overwrites data + clears the source's error" and
    // cross-source-isolation invariants now live in `coordinator::tests`
    // (exercised end-to-end through `StateMsg::Update`), since snapshot
    // mutation moved into the coordinator's `apply_partial`. These tests cover
    // `record_error`, which still lives here.

    #[test]
    fn record_error_preserves_existing_data() {
        let mut s = Snapshot::empty(fixture_now());
        s.claude_oauth = Some(oauth_snapshot());
        record_error(&mut s, Source::ClaudeOAuth, "network unreachable");
        // Data preserved (degraded UI shows stale numbers with warning):
        assert!(s.claude_oauth.is_some());
        // Error recorded:
        assert_eq!(s.claude_oauth_error.as_deref(), Some("network unreachable"));
    }

    #[test]
    fn record_error_clears_oauth_unavailable_marker() {
        // A failed OAuth fetch after a prior "Claude Code not detected" must
        // clear the neutral marker - the two states are mutually exclusive.
        let mut s = Snapshot::empty(fixture_now());
        record_oauth_unavailable(&mut s, "Claude Code not detected");
        record_error(&mut s, Source::ClaudeOAuth, "network unreachable");
        assert_eq!(s.claude_oauth_error.as_deref(), Some("network unreachable"));
        assert!(
            s.claude_oauth_unavailable.is_none(),
            "an error must clear the not-configured marker"
        );
    }

    #[test]
    fn record_error_routes_to_correct_source_slot() {
        let mut s = Snapshot::empty(fixture_now());
        record_error(&mut s, Source::ClaudeJsonl, "jsonl err");
        record_error(&mut s, Source::OpenAiCosts, "openai err");
        record_error(&mut s, Source::AnthropicApiCost, "price err");
        record_error(&mut s, Source::CodexQuota, "codex err");
        assert_eq!(s.claude_jsonl_error.as_deref(), Some("jsonl err"));
        assert_eq!(s.openai_error.as_deref(), Some("openai err"));
        assert_eq!(s.anthropic_api_cost_error.as_deref(), Some("price err"));
        assert_eq!(s.codex_quota_error.as_deref(), Some("codex err"));
        assert!(
            s.claude_oauth_error.is_none(),
            "untouched source stays clean"
        );
    }

    #[test]
    fn record_error_routes_claude_statusline() {
        let mut s = Snapshot::empty(fixture_now());
        record_error(&mut s, Source::ClaudeStatusline, "schema drift v2");
        assert_eq!(
            s.claude_statusline_error.as_deref(),
            Some("schema drift v2")
        );
        assert!(s.claude_statusline.is_none());
    }

    #[test]
    fn record_oauth_unavailable_sets_marker_and_clears_data_error() {
        let mut s = Snapshot::empty(fixture_now());
        s.claude_oauth = Some(oauth_snapshot());
        record_error(&mut s, Source::ClaudeOAuth, "stale fetch");
        record_oauth_unavailable(&mut s, "Claude Code not detected");
        assert_eq!(
            s.claude_oauth_unavailable.as_deref(),
            Some("Claude Code not detected")
        );
        assert!(s.claude_oauth.is_none(), "unavailable clears stale data");
        assert!(
            s.claude_oauth_error.is_none(),
            "unavailable is not an error - the error slot is cleared"
        );
    }

    #[test]
    fn clear_source_clears_oauth_unavailable_marker() {
        let mut s = Snapshot::empty(fixture_now());
        record_oauth_unavailable(&mut s, "Claude Code not detected");
        clear_source(&mut s, Source::ClaudeOAuth);
        assert!(s.claude_oauth_unavailable.is_none());
    }

    // --- pace_for_oauth ---

    /// Build a `ClaudeOAuthSnapshot` from a compact `(key, util_pct, resets_at)`
    /// tuple list. Keeps test bodies short; non-cadence fields are filled with
    /// sensible defaults that don't affect `pace_for_oauth`.
    fn make_oauth(cadences: &[(&str, f32, DateTime<Utc>)]) -> ClaudeOAuthSnapshot {
        ClaudeOAuthSnapshot {
            cadences: cadences
                .iter()
                .map(|(key, util, resets)| CadenceBar {
                    key: key.to_string(),
                    display_label: key.to_string(),
                    utilization_percent: *util,
                    resets_at: *resets,
                })
                .collect(),
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: fixture_now(),
        }
    }

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn pace_for_oauth_empty_cadences_returns_empty_vec() {
        let oauth = make_oauth(&[]);
        let result = pace_for_oauth(&oauth, fixture_now());
        assert!(result.is_empty(), "no cadences → empty pace vec");
    }

    #[test]
    fn pace_for_oauth_unknown_key_is_skipped() {
        // "monthly" is not in `window_len_for`, so it produces no entry.
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now + chrono::Duration::hours(5);
        let oauth = make_oauth(&[("monthly", 50.0, resets_at)]);
        let result = pace_for_oauth(&oauth, now);
        assert!(
            result.is_empty(),
            "unknown cadence key must be filtered out; got {result:?}"
        );
    }

    #[test]
    fn pace_for_oauth_maps_model_specific_seven_day_variants() {
        // Real Max accounts report model-specific 7-day cadences (e.g.
        // `seven_day_sonnet`), NOT a bare `seven_day`. They must still map to
        // the 7-day window, not be dropped.
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now + chrono::Duration::days(7)
            - chrono::Duration::days(3)
            - chrono::Duration::hours(12); // 50% elapsed
        let oauth = make_oauth(&[("seven_day_sonnet", 25.0, resets_at)]);
        let result = pace_for_oauth(&oauth, now);
        assert_eq!(
            result.len(),
            1,
            "seven_day_sonnet must produce a pace entry"
        );
        assert_eq!(result[0].key, "seven_day_sonnet");
        assert!(
            (result[0].elapsed_fraction - 0.5).abs() < 1e-9,
            "must use the 7-day window length"
        );
    }

    #[test]
    fn pace_for_oauth_five_hour_and_seven_day_produce_two_entries() {
        // now = 2026-06-02 12:00:00 UTC
        // five_hour: resets in 3h → window_start = now - 2h → 40% elapsed; 82% used.
        // seven_day: resets in 3.5d → window_start = now - 3.5d → 50% elapsed; 25% used.
        let now = t("2026-06-02T12:00:00Z");
        let five_resets = now + chrono::Duration::hours(3);
        let seven_resets = now + chrono::Duration::days(3) + chrono::Duration::hours(12);

        let oauth = make_oauth(&[
            ("five_hour", 82.0, five_resets),
            ("seven_day", 25.0, seven_resets),
        ]);
        let result = pace_for_oauth(&oauth, now);

        assert_eq!(result.len(), 2, "two known keys → two entries");

        let five = result.iter().find(|wp| wp.key == "five_hour").unwrap();
        let seven = result.iter().find(|wp| wp.key == "seven_day").unwrap();

        // five_hour: 2h elapsed out of 5h → 40% elapsed; 82% used.
        let expected_five = pace(82.0, five_resets, DEFAULT_WINDOW, now);
        assert!(
            (five.used_fraction - expected_five.used_fraction).abs() < 1e-9,
            "five_hour used_fraction mismatch"
        );
        assert!(
            (five.elapsed_fraction - expected_five.elapsed_fraction).abs() < 1e-9,
            "five_hour elapsed_fraction mismatch: got {}, expected {}",
            five.elapsed_fraction,
            expected_five.elapsed_fraction
        );
        assert_eq!(
            five.ratio.is_some(),
            expected_five.ratio.is_some(),
            "five_hour ratio presence must match"
        );
        if let (Some(got), Some(exp)) = (five.ratio, expected_five.ratio) {
            assert!((got - exp).abs() < 1e-6, "five_hour ratio mismatch");
        }

        // seven_day: 3.5 days elapsed out of 7 → 50% elapsed; 25% used.
        let expected_seven = pace(25.0, seven_resets, SEVEN_DAY_WINDOW, now);
        assert!(
            (seven.used_fraction - expected_seven.used_fraction).abs() < 1e-9,
            "seven_day used_fraction mismatch"
        );
        assert!(
            (seven.elapsed_fraction - expected_seven.elapsed_fraction).abs() < 1e-9,
            "seven_day elapsed_fraction mismatch"
        );
    }
}
