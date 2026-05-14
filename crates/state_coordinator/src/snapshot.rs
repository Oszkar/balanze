//! The in-memory `Snapshot` and its pure merge functions.
//!
//! `Snapshot` is the canonical "everything Balanze currently knows" shape.
//! Both `balanze_cli` (single-shot) and the future `src-tauri` (long-running
//! daemon, via the coordinator actor) produce values of this type. Per
//! AGENTS.md §4 #8, identical inputs must yield identical `Snapshot`s.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, Utc};
use openai_client::OpenAiCosts;
use serde::{Deserialize, Serialize};
use window::WindowSummary;

use crate::messages::{Source, SourcePartial};

/// Canonical Balanze state. `None` fields = "not yet observed"; `*_error`
/// fields hold the most recent failure for that source. Successful data and
/// an error can coexist: the data stays (stale) while the error explains
/// why it isn't fresh.
// PartialEq is intentionally NOT derived: `ClaudeOAuthSnapshot` doesn't
// implement it, and the upstream change to add it would force a float-equality
// debate that doesn't pay off. Tests compare individual fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub fetched_at: DateTime<Utc>,
    pub claude_oauth: Option<ClaudeOAuthSnapshot>,
    pub claude_oauth_error: Option<String>,
    pub claude_jsonl: Option<JsonlSnapshot>,
    pub claude_jsonl_error: Option<String>,
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
    pub files_scanned: usize,
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
        Source::OpenAiCosts => &mut snapshot.openai_error,
    };
    *slot = Some(error.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture_now, jsonl_snapshot, oauth_snapshot, openai_costs};

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
        assert_eq!(s.claude_jsonl_error.as_deref(), Some("jsonl err"));
        assert_eq!(s.openai_error.as_deref(), Some("openai err"));
        assert!(s.claude_oauth_error.is_none());
    }
}
