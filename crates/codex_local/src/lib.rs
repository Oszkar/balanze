//! Reads the user's local OpenAI Codex CLI session files
//! (`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`) and extracts
//! the latest rate-limit quota snapshot.
//!
//! Sits in the "data-source crate" tier alongside `claude_parser` and
//! `openai_client`. Unlike `claude_parser`, the output is a single
//! [`CodexQuotaSnapshot`] (not a stream of events) because the Codex
//! 4-quadrant matrix cell needs ONE number - the latest rate-limit
//! utilization. See `SCHEMA-NOTES.md` (in this crate) for the schema
//! investigation that established this design and the field-by-field schema.
//!
//! # Public API
//!
//! - [`read_codex_quota`] - the one-stop entry point: walks the
//!   default Codex sessions directory, finds the latest session file,
//!   parses the most recent `token_count` event, returns
//!   `Option<CodexQuotaSnapshot>`.
//! - [`find_codex_sessions_dir`] / [`find_latest_session`] /
//!   [`read_latest_quota_snapshot`] - the three components if you need
//!   to plumb things differently (e.g., point at a specific session
//!   file for testing).
//!
//! # Failure modes
//!
//! Every fallible function returns `Result<_, ParseError>`. The four
//! outcomes are designed to map cleanly into the eventual
//! `state_coordinator::DegradedState` enum (per AGENTS.md §3.2; the
//! enum itself lands when state_coordinator is wired
//! to consume codex_local's output):
//!
//! - `Err(FileMissing)` - Codex CLI isn't installed (sessions
//!   directory absent). Caller treats as "Codex data not available";
//!   the Codex matrix cell shows as "not configured".
//! - `Err(IoError)` - filesystem error (permission denied, disk
//!   failure) on a directory or file that DID exist. Loud signal;
//!   caller surfaces an error state rather than silently degrading.
//! - `Err(SchemaDrift)` - file(s) contained `token_count` event(s)
//!   but every one of them had unexpected shape. Codex CLI likely
//!   shipped a breaking schema change. Caller surfaces "Codex data
//!   temporarily unavailable" + the path/line in the error so the
//!   maintainer knows where to start debugging.
//! - `Ok(None)` - everything I/O worked, but the latest session had
//!   zero parseable `token_count` events (e.g. session crashed before
//!   quota accounting fired). NOT a drift signal; just no data yet.
//! - `Ok(Some(snap))` - the happy path.
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
pub use walker::{
    CODEX_CONFIG_DIR_ENV, collect_sessions_newest_first, find_codex_sessions_dir,
    find_latest_session,
};

/// One-stop convenience: resolve the Codex sessions directory, walk rollout
/// files newest-first, parse each until one yields a `token_count` snapshot,
/// return it.
///
/// Returns `Ok(None)` if Codex is installed but every rollout file lacks a
/// parseable `token_count` event (or no rollout files exist at all).
/// Returns `Err(FileMissing)` if Codex isn't installed at all. Surfaces the
/// first `SchemaDrift` / `IoError` from the newest file that hit it, instead
/// of silently masking a drift signal behind older data.
///
/// Walking older sessions matters at day-rollover and after fresh `codex`
/// invocations: a brand-new session file exists but hasn't logged a
/// `token_count` yet, while yesterday's session still carries valid 7-day
/// quota state.
pub fn read_codex_quota() -> Result<Option<CodexQuotaSnapshot>, ParseError> {
    let dir = find_codex_sessions_dir()?;
    match try_latest_quota_snapshot(&dir)? {
        LatestQuotaProbe::HasSnapshot(snap) => return Ok(Some(snap)),
        LatestQuotaProbe::NoSessions => return Ok(None),
        LatestQuotaProbe::NoSnapshotInLatest => {}
    }
    let sessions = collect_sessions_newest_first(&dir)?;
    for path in sessions {
        // ? propagates SchemaDrift / IoError immediately so a Codex schema
        // change isn't hidden behind older sessions. Ok(None) ("session has
        // no token_count yet") falls through to the next-older candidate.
        if let Some(snap) = read_latest_quota_snapshot(&path)? {
            return Ok(Some(snap));
        }
    }
    Ok(None)
}

#[derive(Debug, PartialEq)]
enum LatestQuotaProbe {
    HasSnapshot(CodexQuotaSnapshot),
    NoSnapshotInLatest,
    NoSessions,
}

fn try_latest_quota_snapshot(dir: &std::path::Path) -> Result<LatestQuotaProbe, ParseError> {
    let Some(path) = find_latest_session(dir)? else {
        return Ok(LatestQuotaProbe::NoSessions);
    };
    match read_latest_quota_snapshot(&path)? {
        Some(snap) => Ok(LatestQuotaProbe::HasSnapshot(snap)),
        None => Ok(LatestQuotaProbe::NoSnapshotInLatest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    const SESSION_META: &str = r#"{"timestamp":"2026-05-14T06:20:00Z","type":"session_meta","payload":{"id":"session-fast-path","cwd":"/tmp/project"}}"#;
    const TOKEN_COUNT_3PCT: &str = r#"{"timestamp":"2026-05-14T06:23:25.393Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":29331}},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":3.0,"window_minutes":10080,"resets_at":1779344602},"secondary":null,"credits":null,"plan_type":"go","rate_limit_reached_type":null}}}"#;
    const TOKEN_COUNT_5PCT: &str = r#"{"timestamp":"2026-05-14T07:05:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":{"limit_id":"codex","primary":{"used_percent":5.0,"window_minutes":10080,"resets_at":1779344607},"secondary":null,"plan_type":"go","rate_limit_reached_type":null}}}"#;

    fn touch_jsonl(path: &Path, content: &str, mtime_offset_secs: i64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        let now = SystemTime::now();
        let target = if mtime_offset_secs >= 0 {
            now + Duration::from_secs(mtime_offset_secs as u64)
        } else {
            now - Duration::from_secs((-mtime_offset_secs) as u64)
        };
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(target).unwrap();
    }

    #[test]
    fn latest_quota_snapshot_shortcut_reads_newest_session() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch_jsonl(
            &root.join("2026/05/14/rollout-older.jsonl"),
            &format!("{SESSION_META}\n{TOKEN_COUNT_3PCT}\n"),
            -3600,
        );
        touch_jsonl(
            &root.join("2026/05/15/rollout-newer.jsonl"),
            &format!("{SESSION_META}\n{TOKEN_COUNT_5PCT}\n"),
            -60,
        );

        match try_latest_quota_snapshot(root).expect("latest shortcut succeeds") {
            LatestQuotaProbe::HasSnapshot(snap) => assert_eq!(snap.primary.used_percent, 5.0),
            other => panic!("expected latest quota snapshot, got {other:?}"),
        }
    }
}
