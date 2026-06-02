//! The in-memory `Snapshot` and its pure merge functions.
//!
//! `Snapshot` is the canonical "everything Balanze currently knows" shape.
//! Both `balanze_cli` (single-shot) and the future `src-tauri` (long-running
//! daemon, via the coordinator actor) produce values of this type. Per
//! AGENTS.md §4 #8, identical inputs must yield identical `Snapshot`s.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, Utc};
use claude_cost::Cost;
use claude_statusline::StatuslineFilePayload;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use predictor::Prediction;
use serde::{Deserialize, Serialize};
use window::WindowSummary;

use crate::messages::Source;

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
/// both Anthropic cells, `claude_statusline` from the statusLine file
/// payload, and `prediction` from the EWMA predictor.
// PartialEq is intentionally NOT derived: `ClaudeOAuthSnapshot` doesn't
// implement it, and the upstream change to add it would force a float-equality
// debate that doesn't pay off. Tests compare individual fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Wall-clock time of the most recent coordinator-side merge. Updated on
    /// every successful `Update` message.
    pub fetched_at: DateTime<Utc>,
    /// Most recent successful Anthropic OAuth usage fetch. `None` until the
    /// first success.
    pub claude_oauth: Option<ClaudeOAuthSnapshot>,
    /// Most recent failure from the Anthropic OAuth poller. Coexists with
    /// `claude_oauth` when a previously-good fetch is now stale.
    pub claude_oauth_error: Option<String>,
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
    /// Most recent EWMA-based prediction computed from OAuth cadence history.
    /// Recomputed by the coordinator after each successful `ClaudeOAuth` or
    /// `ClaudeJsonl` merge. `None` until the first OAuth merge with a
    /// `five_hour` cadence.
    pub prediction: Option<Prediction>,
}

impl Snapshot {
    /// An empty snapshot stamped with `fetched_at = now`. Used at coordinator
    /// startup before any source has reported in.
    pub fn empty(now: DateTime<Utc>) -> Self {
        Self {
            fetched_at: now,
            claude_oauth: None,
            claude_oauth_error: None,
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
            prediction: None,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture_now, oauth_snapshot};

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
}
