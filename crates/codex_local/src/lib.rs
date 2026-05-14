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
//! Every fallible function returns `Result<_, ParseError>`. The three
//! variants map cleanly to `state_coordinator`'s `DegradedState`:
//!
//! - `FileMissing` → `DegradedState::CodexDirMissing` (Codex isn't
//!   installed, or installed but no sessions yet)
//! - `IoError` → `DegradedState::IoError` (something unusual; surface
//!   loudly)
//! - `SchemaDrift` → `DegradedState::SchemaDrift` (Codex CLI shipped a
//!   breaking change; surface "Codex data temporarily unavailable")
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
