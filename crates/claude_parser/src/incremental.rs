//! Stateful byte-cursor incremental reader for Claude Code JSONL files.
//!
//! Tracks a per-file `(byte_pos, size, mtime)` cursor so subsequent reads
//! only parse newly-appended bytes. Designed for the future watcher /
//! state_coordinator to consume; the CLI is stateless and re-parses fully on
//! each invocation, so it doesn't use this directly.
//!
//! Limitations:
//! - "Atomic rewrite to a strictly larger size" is not detected (the new
//!   file looks like a normal append). In practice Claude Code only appends
//!   to JSONLs, so this case doesn't arise on real data.
//! - Detection is mtime + size based, not content-hash. Truncation and
//!   same-size rewrites ARE detected.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::parser::parse_str;
use crate::types::{ParseError, UsageEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCursor {
    pub byte_pos: u64,
    pub size: u64,
    pub mtime: SystemTime,
}

#[derive(Debug, Default)]
pub struct IncrementalParser {
    cursors: HashMap<PathBuf, FileCursor>,
}

impl IncrementalParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read and parse new bytes appended to `path` since the last call.
    ///
    /// First call (or after `invalidate` / `clear`) reads the whole file.
    /// On a detected truncation (size shrank below the previous cursor) or
    /// same-size atomic rewrite (size unchanged but mtime advanced), the
    /// cursor is reset to 0 and the file is re-read in full.
    ///
    /// Partial trailing lines (no terminating `\n`) are excluded; the cursor
    /// is parked at the start of the partial line so the next call picks it
    /// up once the writer finishes.
    pub fn read_incremental(&mut self, path: &Path) -> Result<Vec<UsageEvent>, ParseError> {
        let metadata = fs::metadata(path).map_err(|e| io_to_parse_err(path, e))?;
        let current_size = metadata.len();
        let current_mtime = metadata.modified().map_err(|e| io_to_parse_err(path, e))?;

        let start_offset = next_start_offset(self.cursors.get(path), current_size, current_mtime);

        let (events, parsed_byte_count) = if start_offset >= current_size {
            (Vec::new(), 0usize)
        } else {
            let mut file = fs::File::open(path).map_err(|e| io_to_parse_err(path, e))?;
            file.seek(SeekFrom::Start(start_offset))
                .map_err(|e| io_to_parse_err(path, e))?;
            let mut buf = String::new();
            file.read_to_string(&mut buf)
                .map_err(|e| io_to_parse_err(path, e))?;
            let prefix_len = complete_prefix_len(&buf);
            let events = if prefix_len == 0 {
                Vec::new()
            } else {
                parse_str(&buf[..prefix_len])?
            };
            (events, prefix_len)
        };

        // Cursor advances to the end of the last complete line. A partial
        // trailing line leaves the cursor parked at its start so the next
        // call retries once the writer finishes.
        let new_byte_pos = start_offset + parsed_byte_count as u64;
        self.cursors.insert(
            path.to_path_buf(),
            FileCursor {
                byte_pos: new_byte_pos,
                size: current_size,
                mtime: current_mtime,
            },
        );

        Ok(events)
    }

    /// Forget cursor state for one file. Next read starts from byte 0.
    pub fn invalidate(&mut self, path: &Path) {
        self.cursors.remove(path);
    }

    /// Forget all cursor state.
    pub fn clear(&mut self) {
        self.cursors.clear();
    }

    /// Inspect the cursor for a given file (for diagnostics / tests).
    pub fn cursor_for(&self, path: &Path) -> Option<&FileCursor> {
        self.cursors.get(path)
    }
}

fn io_to_parse_err(path: &Path, e: std::io::Error) -> ParseError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ParseError::FileMissing(path.to_path_buf()),
        std::io::ErrorKind::PermissionDenied => ParseError::PermissionDenied(path.to_path_buf()),
        _ => ParseError::IoError {
            path: path.to_path_buf(),
            source: e,
        },
    }
}

/// Decide where to start reading `path` on this call.
///
/// Cases:
/// - `None` cursor → read from 0 (first time we've seen this file).
/// - Cursor exists and `current_size < cursor.byte_pos` → truncation; read from 0.
/// - Cursor exists, `current_size == cursor.size`, and `mtime != cursor.mtime`
///   → same-size atomic rewrite; read from 0.
/// - Otherwise → resume from `cursor.byte_pos`. If `current_size == cursor.byte_pos`,
///   the caller will read 0 bytes; the cursor is just refreshed.
fn next_start_offset(
    cursor: Option<&FileCursor>,
    current_size: u64,
    current_mtime: SystemTime,
) -> u64 {
    let Some(c) = cursor else { return 0 };
    if current_size < c.byte_pos {
        return 0;
    }
    if current_size == c.size && current_mtime != c.mtime {
        return 0;
    }
    c.byte_pos
}

/// Byte length of the longest `\n`-terminated prefix of `buf`.
/// Used both for parsing and for advancing the byte cursor.
fn complete_prefix_len(buf: &str) -> usize {
    match buf.rfind('\n') {
        Some(idx) => idx + 1, // include the newline itself
        None => 0,            // no complete line yet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn mtime_at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn cursor(byte_pos: u64, size: u64, mtime_secs: u64) -> FileCursor {
        FileCursor {
            byte_pos,
            size,
            mtime: mtime_at(mtime_secs),
        }
    }

    // --- pure decision logic: next_start_offset ---

    #[test]
    fn no_cursor_starts_at_zero() {
        assert_eq!(next_start_offset(None, 1000, mtime_at(1)), 0);
    }

    #[test]
    fn unchanged_file_resumes_at_byte_pos() {
        let c = cursor(500, 500, 100);
        assert_eq!(next_start_offset(Some(&c), 500, mtime_at(100)), 500);
    }

    #[test]
    fn grown_file_resumes_at_byte_pos() {
        let c = cursor(500, 500, 100);
        assert_eq!(next_start_offset(Some(&c), 800, mtime_at(150)), 500);
    }

    #[test]
    fn truncation_resets_to_zero() {
        let c = cursor(500, 500, 100);
        assert_eq!(next_start_offset(Some(&c), 100, mtime_at(200)), 0);
    }

    #[test]
    fn same_size_rewrite_resets_to_zero() {
        let c = cursor(500, 500, 100);
        // Size unchanged, mtime advanced → file was rewritten with the same length.
        assert_eq!(next_start_offset(Some(&c), 500, mtime_at(200)), 0);
    }

    #[test]
    fn same_size_same_mtime_resumes_at_byte_pos() {
        let c = cursor(500, 500, 100);
        // Nothing actually changed; just resume.
        assert_eq!(next_start_offset(Some(&c), 500, mtime_at(100)), 500);
    }

    // --- complete_prefix_len: partial-line handling ---

    #[test]
    fn complete_prefix_len_empty_buf_is_zero() {
        assert_eq!(complete_prefix_len(""), 0);
    }

    #[test]
    fn complete_prefix_len_one_complete_line() {
        assert_eq!(complete_prefix_len("hello\n"), 6);
    }

    #[test]
    fn complete_prefix_len_partial_only_is_zero() {
        assert_eq!(complete_prefix_len("partial without newline"), 0);
    }

    #[test]
    fn complete_prefix_len_complete_then_partial_stops_after_complete() {
        let buf = "complete\nstill partial";
        assert_eq!(complete_prefix_len(buf), 9); // "complete\n" len = 9
    }

    #[test]
    fn complete_prefix_len_multiple_complete_returns_last_newline_plus_one() {
        let buf = "a\nb\nc\n";
        assert_eq!(complete_prefix_len(buf), 6);
    }

    // --- integration: real tempfile cycles ---

    fn write_assistant_line(content: &mut String, msg_id: &str, req_id: &str, output: u64) {
        // Minimal valid JSONL line that parser accepts.
        let line = format!(
            r#"{{"type":"assistant","timestamp":"2026-05-06T14:28:06.800Z","requestId":"{req_id}","message":{{"id":"{msg_id}","model":"m","usage":{{"input_tokens":1,"output_tokens":{output}}}}}}}"#
        );
        content.push_str(&line);
        content.push('\n');
    }

    #[test]
    fn first_read_returns_all_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        write_assistant_line(&mut content, "msg_b", "req_2", 200);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let events = p.read_incremental(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(p.cursor_for(&path).unwrap().byte_pos, content.len() as u64);
    }

    #[test]
    fn unchanged_file_returns_no_new_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let first = p.read_incremental(&path).unwrap();
        assert_eq!(first.len(), 1);
        let second = p.read_incremental(&path).unwrap();
        assert!(second.is_empty(), "second call must return no new events");
    }

    #[test]
    fn append_returns_only_new_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let first = p.read_incremental(&path).unwrap();
        assert_eq!(first.len(), 1);

        write_assistant_line(&mut content, "msg_b", "req_2", 200);
        write_assistant_line(&mut content, "msg_c", "req_3", 300);
        std::fs::write(&path, &content).unwrap();

        let second = p.read_incremental(&path).unwrap();
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].message_id.as_deref(), Some("msg_b"));
        assert_eq!(second[1].message_id.as_deref(), Some("msg_c"));
    }

    #[test]
    fn partial_trailing_line_is_not_parsed_until_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        // Append a partial line (no trailing newline). Truncated mid-string
        // so the JSON itself is malformed if you tried to parse it now.
        let partial_prefix = r#"{"type":"assistant","timestamp":"2026-05-06T14:29:00"#;
        content.push_str(partial_prefix);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let first = p.read_incremental(&path).unwrap();
        assert_eq!(first.len(), 1, "partial line must be skipped");
        let cursor_after_first = p.cursor_for(&path).unwrap().byte_pos;
        assert!(
            cursor_after_first < content.len() as u64,
            "cursor should be parked before the partial line"
        );

        // Now complete the partial line with a valid suffix + newline.
        let mut completed = content.clone();
        let complete_suffix = r#".000Z","requestId":"req_2","message":{"id":"msg_b","model":"m","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        completed.push_str(complete_suffix);
        completed.push('\n');
        std::fs::write(&path, &completed).unwrap();

        let second = p.read_incremental(&path).unwrap();
        assert_eq!(second.len(), 1, "completed line should parse now");
        assert_eq!(second[0].message_id.as_deref(), Some("msg_b"));
        assert_eq!(second[0].request_id.as_deref(), Some("req_2"));
    }

    #[test]
    fn truncation_re_reads_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        write_assistant_line(&mut content, "msg_b", "req_2", 200);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let first = p.read_incremental(&path).unwrap();
        assert_eq!(first.len(), 2);

        // Truncate: rewrite with just one (shorter) line.
        let mut truncated = String::new();
        write_assistant_line(&mut truncated, "msg_x", "req_99", 999);
        std::fs::write(&path, &truncated).unwrap();

        let second = p.read_incremental(&path).unwrap();
        assert_eq!(second.len(), 1, "truncation should restart from byte 0");
        assert_eq!(second[0].message_id.as_deref(), Some("msg_x"));
    }

    #[test]
    fn invalidate_forces_full_re_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let first = p.read_incremental(&path).unwrap();
        assert_eq!(first.len(), 1);

        p.invalidate(&path);
        let second = p.read_incremental(&path).unwrap();
        assert_eq!(second.len(), 1, "invalidate should re-read from byte 0");
    }

    #[test]
    fn missing_file_returns_file_missing_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        let mut p = IncrementalParser::new();
        match p.read_incremental(&path) {
            Err(ParseError::FileMissing(p)) => assert_eq!(p, path),
            other => panic!("expected FileMissing, got {other:?}"),
        }
    }
}
