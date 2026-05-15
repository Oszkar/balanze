//! The in-memory `Snapshot` and its pure merge functions.
//!
//! `Snapshot` is the canonical "everything Balanze currently knows" shape.
//! Both `balanze_cli` (single-shot) and the future `src-tauri` (long-running
//! daemon, via the coordinator actor) produce values of this type. Per
//! AGENTS.md §4 #8, identical inputs must yield identical `Snapshot`s.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, Utc};
use claude_cost::Cost;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use serde::{Deserialize, Serialize};
use window::WindowSummary;

use crate::messages::{Source, SourcePartial};

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
/// both Anthropic cells.
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

/// Merge a successful source update into the snapshot.
///
/// A successful update overwrites the source's data slot AND clears its
/// `_error` slot — the fetch succeeded, so the previous error (if any) is no
/// longer relevant.
pub fn merge_partial(snapshot: &mut Snapshot, partial: SourcePartial) {
    match partial {
        SourcePartial::ClaudeOAuth(o) => {
            snapshot.claude_oauth = Some(o);
            snapshot.claude_oauth_error = None;
        }
        SourcePartial::ClaudeJsonl(j) => {
            snapshot.claude_jsonl = Some(j);
            snapshot.claude_jsonl_error = None;
        }
        SourcePartial::AnthropicApiCost(c) => {
            snapshot.anthropic_api_cost = Some(c);
            snapshot.anthropic_api_cost_error = None;
        }
        SourcePartial::CodexQuota(q) => {
            snapshot.codex_quota = Some(q);
            snapshot.codex_quota_error = None;
        }
        SourcePartial::OpenAiCosts(c) => {
            snapshot.openai = Some(c);
            snapshot.openai_error = None;
        }
    }
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
    };
    *slot = Some(error.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        anthropic_api_cost, codex_quota, fixture_now, jsonl_snapshot, oauth_snapshot, openai_costs,
    };

    #[test]
    fn merge_oauth_overwrites_data_and_clears_error() {
        let mut s = Snapshot::empty(fixture_now());
        s.claude_oauth_error = Some("previous fetch failed".to_string());
        merge_partial(&mut s, SourcePartial::ClaudeOAuth(oauth_snapshot()));
        assert!(s.claude_oauth.is_some());
        assert!(s.claude_oauth_error.is_none(), "success clears prior error");
    }

    #[test]
    fn merge_jsonl_overwrites_data_and_clears_error() {
        let mut s = Snapshot::empty(fixture_now());
        s.claude_jsonl_error = Some("permission denied".to_string());
        merge_partial(&mut s, SourcePartial::ClaudeJsonl(jsonl_snapshot()));
        assert!(s.claude_jsonl.is_some());
        assert!(s.claude_jsonl_error.is_none());
    }

    #[test]
    fn merge_openai_overwrites_data_and_clears_error() {
        let mut s = Snapshot::empty(fixture_now());
        s.openai_error = Some("401".to_string());
        merge_partial(&mut s, SourcePartial::OpenAiCosts(openai_costs()));
        assert!(s.openai.is_some());
        assert!(s.openai_error.is_none());
    }

    #[test]
    fn merge_anthropic_api_cost_overwrites_data_and_clears_error() {
        let mut s = Snapshot::empty(fixture_now());
        s.anthropic_api_cost_error = Some("price table corrupt".to_string());
        merge_partial(
            &mut s,
            SourcePartial::AnthropicApiCost(anthropic_api_cost()),
        );
        assert!(s.anthropic_api_cost.is_some());
        assert!(s.anthropic_api_cost_error.is_none());
    }

    #[test]
    fn merge_codex_quota_overwrites_data_and_clears_error() {
        let mut s = Snapshot::empty(fixture_now());
        s.codex_quota_error = Some("schema drift".to_string());
        merge_partial(&mut s, SourcePartial::CodexQuota(codex_quota()));
        assert!(s.codex_quota.is_some());
        assert!(s.codex_quota_error.is_none());
    }

    #[test]
    fn record_error_preserves_existing_data() {
        let mut s = Snapshot::empty(fixture_now());
        merge_partial(&mut s, SourcePartial::ClaudeOAuth(oauth_snapshot()));
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
    fn merge_one_source_does_not_clear_another_sources_error() {
        // Cross-source isolation invariant: a successful merge clears ONLY
        // the merged source's `_error` slot. A mis-routed slot write (e.g. a
        // future refactor that clears the wrong field) would blank an
        // unrelated source's degraded indicator and silently hide a failure.
        let mut s = Snapshot::empty(fixture_now());
        s.openai_error = Some("openai 500".to_string());
        s.claude_jsonl_error = Some("jsonl perm denied".to_string());

        merge_partial(&mut s, SourcePartial::ClaudeJsonl(jsonl_snapshot()));

        assert!(s.claude_jsonl.is_some());
        assert!(
            s.claude_jsonl_error.is_none(),
            "merged source's own error is cleared"
        );
        assert_eq!(
            s.openai_error.as_deref(),
            Some("openai 500"),
            "an unrelated source's error must be left untouched"
        );
    }
}
