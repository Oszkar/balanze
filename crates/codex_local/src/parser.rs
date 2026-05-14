//! Parses a single Codex session JSONL file and extracts the latest
//! `CodexQuotaSnapshot`.
//!
//! Scan strategy: linear pass through the file. The first line is
//! expected to be a `session_meta` (carrying the session UUID); each
//! subsequent line is one event. We accumulate `token_count`
//! event_msgs and return the LAST one parsed — that's the most recent
//! quota state.
//!
//! # Error policy (sharper than "silently tolerate")
//!
//! - **`FileMissing`**: passed path doesn't exist (file deleted
//!   between walk and read, or caller fabricated a path). Mapped from
//!   `std::io::ErrorKind::NotFound` on `File::open`.
//! - **`IoError`**: any other open / read failure (permission denied,
//!   disk error). Loud signal — distinguish "Codex isn't installed"
//!   (FileMissing) from "filesystem is broken" (IoError) for the
//!   caller.
//! - **`SchemaDrift`**: the file contained one or more `token_count`
//!   event_msgs but the parser couldn't extract a valid quota from
//!   any of them — Codex CLI may have shipped a breaking schema
//!   change. Reported with the line number of the last drift event
//!   and a count in the message. Distinct from `Ok(None)` (no
//!   `token_count` events at all — session crashed before quota
//!   accounting fired).
//! - **`Ok(None)`**: the file is well-formed but contains zero
//!   `token_count` event_msgs.
//! - **`Ok(Some(_))`**: at least one `token_count` event parsed
//!   successfully; we return the latest.

use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::errors::ParseError;
use crate::types::{CodexQuotaSnapshot, RateLimitWindow};

/// Read one Codex session file and return the latest rate-limit
/// snapshot. See the module-level "Error policy" doc for the four
/// outcomes and what each means.
pub fn read_latest_quota_snapshot(path: &Path) -> Result<Option<CodexQuotaSnapshot>, ParseError> {
    let file = File::open(path).map_err(|source| {
        if source.kind() == ErrorKind::NotFound {
            ParseError::FileMissing(path.to_path_buf())
        } else {
            ParseError::IoError {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    let reader = BufReader::new(file);

    let mut session_id = String::new();
    let mut latest: Option<CodexQuotaSnapshot> = None;
    // Drift accounting: how many `token_count` event_msgs we saw vs
    // how many we successfully extracted into `latest`. If we saw any
    // attempts but extracted zero, that's a schema-drift signal worth
    // surfacing as a typed error.
    let mut token_count_attempts: usize = 0;
    let mut last_drift_line: usize = 0;

    for (idx, line_result) in reader.lines().enumerate() {
        let line_no = idx + 1; // 1-indexed for human-readable errors
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
            Err(_) => continue, // unparseable line — not a token_count event by definition
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

        // From here on, we're committed: this line is a token_count
        // attempt. Any structural failure below counts as drift.
        token_count_attempts += 1;

        let rate_limits = match payload.get("rate_limits") {
            Some(rl) => rl,
            None => {
                last_drift_line = line_no;
                continue;
            }
        };
        let primary = match parse_window(rate_limits.pointer("/primary")) {
            Some(w) => w,
            None => {
                last_drift_line = line_no;
                continue;
            }
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
                // surface as drift if this was the only candidate.
                last_drift_line = line_no;
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

    match latest {
        Some(snap) => Ok(Some(snap)),
        None if token_count_attempts > 0 => Err(ParseError::SchemaDrift {
            path: path.to_path_buf(),
            line: last_drift_line,
            message: format!(
                "saw {token_count_attempts} token_count event(s) but extracted no valid \
                 quota snapshot — Codex CLI may have shipped a breaking schema change"
            ),
        }),
        None => Ok(None),
    }
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

    #[test]
    fn all_token_count_events_drift_returns_schema_drift_error() {
        // File has ≥1 token_count event_msg but every one of them is
        // missing the primary block. Per the post-review error policy,
        // this is a SchemaDrift signal (Codex CLI likely changed its
        // schema) — distinct from Ok(None) which means "no token_count
        // events at all" (session crashed before quota accounting).
        let drift_a = r#"{"timestamp":"2026-05-14T08:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"plan_type":"go"}}}"#;
        let drift_b = r#"{"timestamp":"2026-05-14T08:05:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"plan_type":"go"}}}"#;
        let f = write_jsonl(&[SESSION_META, drift_a, drift_b]);
        let err = read_latest_quota_snapshot(f.path()).unwrap_err();
        match err {
            ParseError::SchemaDrift { line, message, .. } => {
                assert!(
                    message.contains("saw 2 token_count"),
                    "got message: {message}"
                );
                assert!(
                    message.contains("Codex CLI may have shipped"),
                    "got: {message}"
                );
                // Last drift event was on the third line of the file (1-indexed).
                assert_eq!(line, 3);
            }
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn parses_object_valued_secondary_window() {
        // Higher-tier plans (per Codex docs) populate the secondary
        // window with an object. Pin the parsing path so a regression
        // in the secondary handler is caught.
        let with_secondary = r#"{"timestamp":"2026-05-14T09:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":42.0,"window_minutes":10080,"resets_at":1779344602},"secondary":{"used_percent":7.5,"window_minutes":300,"resets_at":1779260400},"plan_type":"pro","rate_limit_reached_type":null}}}"#;
        let f = write_jsonl(&[SESSION_META, with_secondary]);
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        assert_eq!(snap.primary.used_percent, 42.0);
        assert_eq!(snap.primary.window_duration_minutes, 10080);
        let secondary = snap.secondary.expect("secondary should be Some");
        assert_eq!(secondary.used_percent, 7.5);
        assert_eq!(secondary.window_duration_minutes, 300);
        assert_eq!(snap.plan_type, "pro");
    }

    #[test]
    fn nonexistent_path_returns_file_missing_not_io_error() {
        // File::open's NotFound case must surface as FileMissing per
        // the error contract — callers distinguish "Codex isn't
        // installed" (graceful) from "filesystem is broken" (loud).
        let tmp = tempfile::TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist.jsonl");
        let err = read_latest_quota_snapshot(&nonexistent).unwrap_err();
        match err {
            ParseError::FileMissing(p) => assert_eq!(p, nonexistent),
            other => panic!("expected FileMissing, got {other:?}"),
        }
    }
}
