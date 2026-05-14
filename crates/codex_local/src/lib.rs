//! Reads the user's local OpenAI Codex CLI session files
//! (`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`) and extracts
//! the latest rate-limit quota snapshot.
//!
//! Sits in the "data-source crate" tier alongside `claude_parser` and
//! `openai_client`. Unlike `claude_parser`, the output is a single
//! [`CodexQuotaSnapshot`] (not a stream of events) because the Codex
//! 4-quadrant matrix cell needs ONE number — the latest rate-limit
//! utilization. See `SCHEMA-NOTES.md` (in this crate) for the spike
//! that established this design and the field-by-field schema.
//!
//! # Public API
//!
//! - [`read_codex_quota`] — the one-stop entry point: walks the
//!   default Codex sessions directory, finds the latest session file,
//!   parses the most recent `token_count` event, returns
//!   `Option<CodexQuotaSnapshot>`.
//! - [`find_codex_sessions_dir`] / [`find_latest_session`] /
//!   [`read_latest_quota_snapshot`] — the three components if you need
//!   to plumb things differently (e.g., point at a specific session
//!   file for testing).
//!
//! # Failure modes
//!
//! Every fallible function returns `Result<_, ParseError>`. The four
//! outcomes are designed to map cleanly into the eventual
//! `state_coordinator::DegradedState` enum (per AGENTS.md §3.2; the
//! enum itself lands in v0.1 step 5 when state_coordinator is wired
//! to consume codex_local's output):
//!
//! - `Err(FileMissing)` — Codex CLI isn't installed (sessions
//!   directory absent). Caller treats as "Codex data not available";
//!   the Codex matrix cell shows as "not configured".
//! - `Err(IoError)` — filesystem error (permission denied, disk
//!   failure) on a directory or file that DID exist. Loud signal;
//!   caller surfaces an error state rather than silently degrading.
//! - `Err(SchemaDrift)` — file(s) contained `token_count` event(s)
//!   but every one of them had unexpected shape. Codex CLI likely
//!   shipped a breaking schema change. Caller surfaces "Codex data
//!   temporarily unavailable" + the path/line in the error so the
//!   maintainer knows where to start debugging.
//! - `Ok(None)` — everything I/O worked, but the latest session had
//!   zero parseable `token_count` events (e.g. session crashed before
//!   quota accounting fired). NOT a drift signal; just no data yet.
//! - `Ok(Some(snap))` — the happy path.
//!
//! # `CODEX_CONFIG_DIR`
//!
//! The env var `CODEX_CONFIG_DIR` overrides the default home-dir
//! resolution and is appended with `sessions/` (matches Codex CLI's
//! `$CODEX_HOME` semantic).

pub mod errors;
pub mod parser;
pub mod types;
pub mod walker;

pub use errors::ParseError;
pub use parser::read_latest_quota_snapshot;
pub use types::{CodexQuotaSnapshot, RateLimitWindow};
pub use walker::{find_codex_sessions_dir, find_latest_session, CODEX_CONFIG_DIR_ENV};

/// One-stop convenience: resolve the Codex sessions directory, find
/// the latest session file, parse the most recent `token_count` event,
/// return the snapshot.
///
/// Returns `Ok(None)` if Codex is installed but has produced no
/// session files yet, OR if the latest session file has no parseable
/// `token_count` event. Returns `Err(FileMissing)` if Codex isn't
/// installed at all.
pub fn read_codex_quota() -> Result<Option<CodexQuotaSnapshot>, ParseError> {
    let dir = find_codex_sessions_dir()?;
    let Some(path) = find_latest_session(&dir)? else {
        return Ok(None);
    };
    read_latest_quota_snapshot(&path)
}
