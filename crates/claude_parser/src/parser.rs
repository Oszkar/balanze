use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::types::{AccountType, DataSource, ParseError, Provider, UsageEvent};

/// Raw deserialization target. Only the subset we care about is declared;
/// everything else stays implicit so unrelated line types don't fail parsing.
#[derive(Debug, Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    message: Option<RawMessage>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

/// Parse one JSONL line.
///
/// Returns:
/// - `Ok(Some(event))` for a usage-bearing assistant message.
/// - `Ok(None)` for any other recognized line (session metadata, hooks,
///   user messages, file snapshots, blank lines) — these are intentional skips.
/// - `Err(SchemaDrift)` for invalid JSON or for an assistant line that lacks
///   a top-level timestamp (a real shape violation worth surfacing).
///
/// `line_no` is 1-indexed and only used for the error message; the parser is
/// otherwise stateless.
pub fn parse_line(line: &str, line_no: usize) -> Result<Option<UsageEvent>, ParseError> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    let raw: RawLine = serde_json::from_str(line).map_err(|e| ParseError::SchemaDrift {
        line: line_no,
        message: format!("invalid JSON: {e}"),
    })?;

    if raw.kind.as_deref() != Some("assistant") {
        return Ok(None);
    }

    let Some(message) = raw.message else {
        return Ok(None);
    };
    let Some(usage) = message.usage else {
        return Ok(None);
    };
    let ts = raw.timestamp.ok_or_else(|| ParseError::SchemaDrift {
        line: line_no,
        message: "assistant line missing top-level timestamp".into(),
    })?;

    Ok(Some(UsageEvent {
        ts,
        provider: Provider::Claude,
        account_type: AccountType::Subscription,
        model: message.model.unwrap_or_default(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cost_micro_usd: None,
        source: DataSource::Jsonl,
        message_id: message.id,
        request_id: raw.request_id,
    }))
}

/// Parse every line of a JSONL document into events.
///
/// Errors propagate with the line number that broke parsing; callers may
/// choose to log and skip the file, or to fail fast.
pub fn parse_str(input: &str) -> Result<Vec<UsageEvent>, ParseError> {
    let mut events = Vec::new();
    for (idx, line) in input.lines().enumerate() {
        if let Some(event) = parse_line(line, idx + 1)? {
            events.push(event);
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_non_assistant_lines() {
        let cases = [
            r#"{"type":"last-prompt","sessionId":"x"}"#,
            r#"{"type":"permission-mode","permissionMode":"acceptEdits"}"#,
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"file-history-snapshot","messageId":"x"}"#,
        ];
        for case in cases {
            assert_eq!(parse_line(case, 1).unwrap(), None, "should skip: {case}");
        }
    }

    #[test]
    fn skips_assistant_without_usage() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z","message":{"model":"claude-sonnet-4-5","role":"assistant"}}"#;
        assert_eq!(parse_line(line, 1).unwrap(), None);
    }

    #[test]
    fn parses_full_usage_line() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":1000,"cache_read_input_tokens":5000}}}"#;
        let ev = parse_line(line, 1).unwrap().expect("usage line should parse");
        assert_eq!(ev.model, "claude-sonnet-4-5");
        assert_eq!(ev.input_tokens, 100);
        assert_eq!(ev.output_tokens, 50);
        assert_eq!(ev.cache_creation_input_tokens, 1000);
        assert_eq!(ev.cache_read_input_tokens, 5000);
        assert_eq!(ev.total_tokens(), 6150);
        assert_eq!(ev.provider, Provider::Claude);
        assert_eq!(ev.account_type, AccountType::Subscription);
        assert_eq!(ev.source, DataSource::Jsonl);
        assert!(ev.cost_micro_usd.is_none());
        assert_eq!(ev.ts.to_rfc3339(), "2026-05-06T14:28:06.800+00:00");
    }

    #[test]
    fn parses_usage_with_zero_cache_fields() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":6,"output_tokens":17}}}"#;
        let ev = parse_line(line, 1).unwrap().unwrap();
        assert_eq!(ev.cache_creation_input_tokens, 0);
        assert_eq!(ev.cache_read_input_tokens, 0);
        assert_eq!(ev.total_tokens(), 23);
    }

    #[test]
    fn extracts_message_id_and_request_id_when_present() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z","requestId":"req_011CaztiaTDrx5M77znpr6P5","message":{"id":"msg_01UuzJzVNCC9cgV7A5jAc63X","model":"claude-sonnet-4-6","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        let ev = parse_line(line, 1).unwrap().unwrap();
        assert_eq!(ev.message_id.as_deref(), Some("msg_01UuzJzVNCC9cgV7A5jAc63X"));
        assert_eq!(ev.request_id.as_deref(), Some("req_011CaztiaTDrx5M77znpr6P5"));
    }

    #[test]
    fn missing_ids_become_none_not_parse_error() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        let ev = parse_line(line, 1).unwrap().unwrap();
        assert_eq!(ev.message_id, None);
        assert_eq!(ev.request_id, None);
    }

    #[test]
    fn tolerates_extra_unknown_fields() {
        // The real schema has many fields we don't read (service_tier, iterations,
        // server_tool_use, etc.). Parsing must tolerate them silently.
        let line = r#"{
            "type":"assistant",
            "timestamp":"2026-05-06T14:28:06.800Z",
            "uuid":"abc",
            "sessionId":"def",
            "message":{
                "model":"claude-sonnet-4-5",
                "id":"msg_x",
                "role":"assistant",
                "usage":{
                    "input_tokens":1,
                    "output_tokens":2,
                    "service_tier":"standard",
                    "iterations":[{"input_tokens":1}],
                    "server_tool_use":{"web_search_requests":0}
                }
            },
            "version":"2.1.131"
        }"#;
        let ev = parse_line(line, 1).unwrap().unwrap();
        assert_eq!(ev.input_tokens, 1);
        assert_eq!(ev.output_tokens, 2);
    }

    #[test]
    fn empty_or_whitespace_line_is_ok_none() {
        assert_eq!(parse_line("", 1).unwrap(), None);
        assert_eq!(parse_line("   ", 1).unwrap(), None);
        assert_eq!(parse_line("\t", 1).unwrap(), None);
    }

    #[test]
    fn malformed_json_is_schema_drift_with_line_number() {
        // Truncated mid-string — typical "partial line being written" failure.
        let line = r#"{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z""#;
        match parse_line(line, 42) {
            Err(ParseError::SchemaDrift { line, .. }) => assert_eq!(line, 42),
            other => panic!("expected SchemaDrift on line 42, got {other:?}"),
        }
    }

    #[test]
    fn assistant_missing_timestamp_is_schema_drift() {
        let line = r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        match parse_line(line, 7) {
            Err(ParseError::SchemaDrift { line, message }) => {
                assert_eq!(line, 7);
                assert!(message.contains("timestamp"), "message should mention timestamp: {message}");
            }
            other => panic!("expected SchemaDrift on line 7, got {other:?}"),
        }
    }

    #[test]
    fn parse_str_skips_blanks_and_metadata_keeping_only_usage() {
        let input = "\n\
                     {\"type\":\"last-prompt\",\"sessionId\":\"x\"}\n\
                     {\"type\":\"assistant\",\"timestamp\":\"2026-05-06T14:28:06.800Z\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\
                     {\"type\":\"permission-mode\",\"permissionMode\":\"acceptEdits\"}\n\
                     {\"type\":\"assistant\",\"timestamp\":\"2026-05-06T14:29:00.000Z\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20}}}\n";
        let events = parse_str(input).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].input_tokens, 1);
        assert_eq!(events[1].input_tokens, 10);
    }

    #[test]
    fn parse_str_propagates_error_with_actual_line_number() {
        let input = "\
                     {\"type\":\"last-prompt\"}\n\
                     {\"type\":\"assistant\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n";
        match parse_str(input) {
            Err(ParseError::SchemaDrift { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected SchemaDrift on line 2, got {other:?}"),
        }
    }

    #[test]
    fn partial_last_line_is_schema_drift_not_silent() {
        // Simulates the parser tailing a JSONL file mid-write. The last line
        // is incomplete. The parser must NOT silently drop it — silent drops
        // would hide real schema drift. Caller decides whether to retry.
        let input = "\
                     {\"type\":\"last-prompt\"}\n\
                     {\"type\":\"assistant\",\"timestamp\":\"2026-05-06T14:28:06";
        match parse_str(input) {
            Err(ParseError::SchemaDrift { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected SchemaDrift on line 2, got {other:?}"),
        }
    }
}
