//! Parses a single Codex session JSONL file and extracts the latest
//! `CodexQuotaSnapshot`.
//!
//! Scan strategy: linear pass through the file. The first line is
//! expected to be a `session_meta` (carrying the session UUID); each
//! subsequent line is one event. We accumulate `token_count`
//! event_msgs and return the LAST one parsed — that's the most recent
//! quota state. Schema drift on individual lines is silently tolerated
//! (per AGENTS.md §3.3: degrade gracefully on upstream schema changes)
//! but the function returns `Ok(None)` if zero `token_count` events
//! were parseable.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::errors::ParseError;
use crate::types::{CodexQuotaSnapshot, RateLimitWindow};

/// Read one Codex session file and return the latest rate-limit
/// snapshot extracted from it, or `Ok(None)` if no parseable
/// `token_count` event was present (e.g. session crashed before any
/// token accounting fired).
///
/// File IO errors propagate as `ParseError::IoError`. Per-line schema
/// drift does NOT propagate — the scanner skips malformed lines and
/// continues looking for the latest valid `token_count`. This matches
/// the "best-effort, never panic on upstream drift" stance from
/// AGENTS.md §3.3.
pub fn read_latest_quota_snapshot(path: &Path) -> Result<Option<CodexQuotaSnapshot>, ParseError> {
    let file = File::open(path).map_err(|source| ParseError::IoError {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);

    let mut session_id = String::new();
    let mut latest: Option<CodexQuotaSnapshot> = None;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(source) => {
                return Err(ParseError::IoError {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // schema drift on this line; skip
        };

        // Capture session_id from the session_meta line (typically line 1).
        if session_id.is_empty() && value.get("type") == Some(&Value::String("session_meta".into()))
        {
            if let Some(id) = value.pointer("/payload/id").and_then(|v| v.as_str()) {
                session_id = id.to_string();
            }
            continue;
        }

        // Look for event_msg with payload.type == "token_count".
        if value.get("type") != Some(&Value::String("event_msg".into())) {
            continue;
        }
        let payload = match value.get("payload") {
            Some(p) => p,
            None => continue,
        };
        if payload.get("type") != Some(&Value::String("token_count".into())) {
            continue;
        }

        let rate_limits = match payload.get("rate_limits") {
            Some(rl) => rl,
            None => continue,
        };

        // Parse the primary window. Drift on this is fatal-for-the-line
        // but not fatal-for-the-file; skip and keep scanning.
        let primary = match parse_window(rate_limits.pointer("/primary")) {
            Some(w) => w,
            None => continue,
        };

        let secondary = parse_window(rate_limits.pointer("/secondary"));

        let plan_type = rate_limits
            .get("plan_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let rate_limit_reached = rate_limits
            .get("rate_limit_reached_type")
            .map(|v| !v.is_null())
            .unwrap_or(false);

        // Parse top-level timestamp on the event_msg line.
        let observed_at = match value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
        {
            Some(ts) => ts,
            None => {
                // Drift: malformed/missing timestamp. We could still
                // return the rest of the snapshot with a placeholder,
                // but that would lie about freshness. Better to skip
                // and let the previous (older) snapshot stand, or
                // return None if this was the only candidate.
                continue;
            }
        };

        latest = Some(CodexQuotaSnapshot {
            observed_at,
            session_id: session_id.clone(),
            primary,
            secondary,
            plan_type,
            rate_limit_reached,
        });
        // Don't break — keep scanning so the LAST valid token_count wins.
    }

    Ok(latest)
}

/// Parse a `RateLimitWindow` from a JSON value. Returns `None` on any
/// schema drift (missing field, wrong type) so the caller can decide
/// whether to skip the whole event or continue without this window.
fn parse_window(value: Option<&Value>) -> Option<RateLimitWindow> {
    let obj = value?.as_object()?;
    let used_percent = obj.get("used_percent")?.as_f64()?;
    let window_duration_minutes = obj.get("window_minutes")?.as_u64()?;
    let resets_at_unix = obj.get("resets_at")?.as_i64()?;
    let resets_at = Utc.timestamp_opt(resets_at_unix, 0).single()?;
    Some(RateLimitWindow {
        used_percent,
        window_duration_minutes,
        resets_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Canonical sample line shapes pulled from the spike against real
    /// `~/.codex/sessions/` data. Anonymized: session UUID is the
    /// well-known "00000000-…" pattern.
    const SESSION_META: &str = r#"{"timestamp":"2026-05-14T06:23:20.076Z","type":"session_meta","payload":{"id":"00000000-0000-7000-8000-000000000001","timestamp":"2026-05-14T06:23:10.584Z","cwd":"E:\\test","originator":"codex_exec","cli_version":"0.130.0"}}"#;

    const TOKEN_COUNT_3PCT: &str = r#"{"timestamp":"2026-05-14T06:23:25.393Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":29331}},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":3.0,"window_minutes":10080,"resets_at":1779344602},"secondary":null,"credits":null,"plan_type":"go","rate_limit_reached_type":null}}}"#;

    const TOKEN_COUNT_5PCT: &str = r#"{"timestamp":"2026-05-14T07:05:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":{"limit_id":"codex","primary":{"used_percent":5.0,"window_minutes":10080,"resets_at":1779344607},"secondary":null,"plan_type":"go","rate_limit_reached_type":null}}}"#;

    fn write_jsonl(lines: &[&str]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_canonical_fixture_line() {
        let f = write_jsonl(&[SESSION_META, TOKEN_COUNT_3PCT]);
        let snap = read_latest_quota_snapshot(f.path())
            .unwrap()
            .expect("non-empty");
        assert_eq!(snap.session_id, "00000000-0000-7000-8000-000000000001");
        assert_eq!(snap.primary.used_percent, 3.0);
        assert_eq!(snap.primary.window_duration_minutes, 10080);
        assert!(snap.secondary.is_none());
        assert_eq!(snap.plan_type, "go");
        assert!(!snap.rate_limit_reached);
        // Spot-check timestamp parsing.
        assert_eq!(
            snap.observed_at.to_rfc3339(),
            "2026-05-14T06:23:25.393+00:00"
        );
    }

    #[test]
    fn last_token_count_event_wins() {
        // Two token_count events, 3% then 5%. We want the 5% one.
        let f = write_jsonl(&[SESSION_META, TOKEN_COUNT_3PCT, TOKEN_COUNT_5PCT]);
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        assert_eq!(snap.primary.used_percent, 5.0);
    }

    #[test]
    fn zero_token_count_events_returns_none() {
        // Session_meta only — the rest of the session crashed before any
        // token accounting fired.
        let f = write_jsonl(&[SESSION_META]);
        let snap = read_latest_quota_snapshot(f.path()).unwrap();
        assert!(snap.is_none());
    }

    #[test]
    fn malformed_line_skipped_earlier_events_preserved() {
        // Valid event, malformed line, no second valid event. We should
        // still get the snapshot from the first valid event.
        let malformed = r#"{not valid json,,, eof"#;
        let f = write_jsonl(&[SESSION_META, TOKEN_COUNT_3PCT, malformed]);
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        assert_eq!(snap.primary.used_percent, 3.0);
    }

    #[test]
    fn missing_primary_block_skipped_not_fatal() {
        let no_primary = r#"{"timestamp":"2026-05-14T08:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"plan_type":"go"}}}"#;
        let f = write_jsonl(&[SESSION_META, TOKEN_COUNT_3PCT, no_primary]);
        // The no-primary event is skipped; the 3% event wins.
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        assert_eq!(snap.primary.used_percent, 3.0);
    }

    #[test]
    fn rate_limit_reached_flag_set_when_type_non_null() {
        let reached = r#"{"timestamp":"2026-05-14T09:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":100.0,"window_minutes":10080,"resets_at":1779344608},"plan_type":"go","rate_limit_reached_type":"primary"}}}"#;
        let f = write_jsonl(&[SESSION_META, reached]);
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        assert!(snap.rate_limit_reached);
        assert_eq!(snap.primary.used_percent, 100.0);
    }

    #[test]
    fn missing_session_meta_yields_empty_session_id() {
        // No session_meta line; only a token_count event. Should still
        // return a snapshot, just with empty session_id (defensive
        // behavior — session_id is for traceability, not load-bearing).
        let f = write_jsonl(&[TOKEN_COUNT_3PCT]);
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        assert_eq!(snap.session_id, "");
        assert_eq!(snap.primary.used_percent, 3.0);
    }
}
