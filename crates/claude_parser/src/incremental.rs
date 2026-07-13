//! Stateful byte-cursor incremental reader for Claude Code JSONL files.
//!
//! Tracks a per-file `(byte_pos, size, mtime)` cursor so subsequent reads
//! only parse newly-appended bytes. The result distinguishes append deltas
//! from replacement snapshots so the watcher can maintain per-file ownership;
//! the CLI is stateless and re-parses fully on each invocation.
//!
//! Replacement detection combines file identity, mtime, size, and bounded
//! probes of the committed prefix. File identity catches atomic replacements;
//! metadata catches truncations and same-size rewrites; probes catch growing
//! in-place rewrites that alter the sampled committed prefix without turning
//! each append into a full-file read. A writer that preserves both bounded
//! samples while changing only unsampled committed bytes is indistinguishable
//! from an append without rereading the full prefix.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::parser::parse_str_lossy;
use crate::types::{ParseError, UsageEvent};

/// Snapshot of a single file's state at the last `read_incremental` call.
/// Used by the next call to decide where to resume reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCursor {
    /// Byte offset of the first un-parsed byte. Equal to the end of the last
    /// complete (`\n`-terminated) line we parsed.
    pub byte_pos: u64,
    /// Total file size at the last call. Used together with `mtime` to
    /// detect rewrites (size unchanged + mtime advanced) and shrinkage.
    pub size: u64,
    /// File modification time at the last call.
    pub mtime: SystemTime,
    /// Physical file identity used to distinguish a growing append from an
    /// atomic replacement at the same path.
    identity: Option<FileIdentity>,
    /// Bounded samples of bytes already committed below `byte_pos`. A normal
    /// append cannot change them; a changed sample means the file contribution
    /// must be replaced even when the inode stayed the same and size grew.
    probe: FileProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity(u128);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileProbe {
    head: Vec<u8>,
    tail: Vec<u8>,
}

const PROBE_BYTES: u64 = 64;

/// Events parsed by one incremental read and how the caller must apply them
/// to that file's existing contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncrementalRead {
    /// Newly appended complete lines. Extend the file's existing events.
    Append(Vec<UsageEvent>),
    /// A first read, truncation, rewrite, or explicit invalidation. Replace
    /// the file's existing events with this complete-prefix snapshot.
    Replace(Vec<UsageEvent>),
}

impl IncrementalRead {
    pub fn events(&self) -> &[UsageEvent] {
        match self {
            Self::Append(events) | Self::Replace(events) => events,
        }
    }

    pub fn is_replacement(&self) -> bool {
        matches!(self, Self::Replace(_))
    }
}

impl Deref for IncrementalRead {
    type Target = [UsageEvent];

    fn deref(&self) -> &Self::Target {
        self.events()
    }
}

/// Stateful per-file byte-cursor reader. Construct once, call
/// `read_incremental(path)` repeatedly; only newly-appended bytes are parsed
/// after the first call.
///
/// Per AGENTS.md §3.1, watcher / poller code uses this to keep parser CPU
/// flat during active Claude Code sessions. The CLI is stateless and
/// re-parses fully on each invocation, so it doesn't use this type.
#[derive(Debug, Default)]
pub struct IncrementalParser {
    cursors: HashMap<PathBuf, FileCursor>,
}

impl IncrementalParser {
    /// Create an empty parser with no cursors. First `read_incremental` call
    /// for each path reads that file in full.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read and parse new bytes appended to `path` since the last call.
    ///
    /// First call (or after `invalidate` / `clear`) reads the whole file.
    /// On a detected truncation (size shrank below the previous cursor) or
    /// same-size atomic rewrite (size unchanged but mtime advanced), the
    /// cursor is reset to 0 and the result is [`IncrementalRead::Replace`].
    ///
    /// Partial trailing lines (no terminating `\n`) are excluded at the byte
    /// level before UTF-8 decoding; the cursor parks at the partial line so a
    /// mid-codepoint write is harmless until the writer finishes it.
    pub fn read_incremental(&mut self, path: &Path) -> Result<IncrementalRead, ParseError> {
        let mut file = fs::File::open(path).map_err(|e| io_to_parse_err(path, e))?;
        let metadata = file.metadata().map_err(|e| io_to_parse_err(path, e))?;
        let current_size = metadata.len();
        let current_mtime = metadata.modified().map_err(|e| io_to_parse_err(path, e))?;
        let current_identity = file_identity(&file, &metadata);

        let cursor = self.cursors.get(path);
        let probe_changed = match cursor {
            Some(cursor) if current_size >= cursor.byte_pos => {
                read_probe(&mut file, cursor.byte_pos, path)? != cursor.probe
            }
            _ => false,
        };
        let replacement = requires_replacement(cursor, current_size, current_mtime)
            || identity_changed(cursor, current_identity)
            || probe_changed;
        let start_offset = if replacement {
            0
        } else {
            next_start_offset(cursor, current_size, current_mtime)
        };

        let (events, parsed_byte_count) = if start_offset >= current_size {
            (Vec::new(), 0usize)
        } else {
            file.seek(SeekFrom::Start(start_offset))
                .map_err(|e| io_to_parse_err(path, e))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|e| io_to_parse_err(path, e))?;
            let prefix_len = complete_prefix_len(&buf);
            let events = if prefix_len == 0 {
                Vec::new()
            } else {
                let mut events = Vec::new();
                let mut skipped_lines = 0usize;
                for line in buf[..prefix_len].split_inclusive(|byte| *byte == b'\n') {
                    match std::str::from_utf8(line) {
                        Ok(line) => {
                            let parsed = parse_str_lossy(line);
                            skipped_lines += parsed.skipped_lines.len();
                            events.extend(parsed.events);
                        }
                        Err(_) => skipped_lines += 1,
                    }
                }
                if skipped_lines != 0 {
                    // Skip-and-advance: a schema-drift or malformed *complete*
                    // line must not park the cursor in front of the batch. The
                    // strict `parse_str` would `?`-abort here and, since the
                    // cursor is only advanced below, leave `byte_pos` pinned so
                    // every later read re-hit the same line and stalled all
                    // future appends for this file. We keep the good events and
                    // let the cursor advance past the whole complete prefix.
                    tracing::warn!(
                        "claude_parser: skipped {} unparseable line(s) in {} (batch at byte {}); continuing without them",
                        skipped_lines,
                        path.display(),
                        start_offset,
                    );
                }
                events
            };
            (events, prefix_len)
        };

        // Cursor advances to the end of the last complete line. A partial
        // trailing line leaves the cursor parked at its start so the next
        // call retries once the writer finishes.
        let new_byte_pos = start_offset + parsed_byte_count as u64;
        let probe = read_probe(&mut file, new_byte_pos, path)?;
        self.cursors.insert(
            path.to_path_buf(),
            FileCursor {
                byte_pos: new_byte_pos,
                size: current_size,
                mtime: current_mtime,
                identity: current_identity,
                probe,
            },
        );

        Ok(if replacement {
            IncrementalRead::Replace(events)
        } else {
            IncrementalRead::Append(events)
        })
    }

    /// Forget cursor state for one file. The next `read_incremental` for that
    /// path starts from byte 0. Use when you've detected drift the cursor
    /// can't catch on its own (e.g., a file moved + restored).
    pub fn invalidate(&mut self, path: &Path) {
        self.cursors.remove(path);
    }

    /// Forget all cursor state. Equivalent to dropping and rebuilding the
    /// parser. Useful after a `refresh_now()` from the user.
    pub fn clear(&mut self) {
        self.cursors.clear();
    }

    /// Inspect the cursor for one path. Diagnostic / test surface; not part
    /// of the runtime read path.
    pub fn cursor_for(&self, path: &Path) -> Option<&FileCursor> {
        self.cursors.get(path)
    }
}

fn read_probe(
    file: &mut fs::File,
    committed_end: u64,
    path: &Path,
) -> Result<FileProbe, ParseError> {
    if committed_end == 0 {
        return Ok(FileProbe::default());
    }

    let head_len = committed_end.min(PROBE_BYTES) as usize;
    let mut head = vec![0; head_len];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut head))
        .map_err(|error| io_to_parse_err(path, error))?;

    let tail_start = committed_end.saturating_sub(PROBE_BYTES);
    let mut tail = vec![0; (committed_end - tail_start) as usize];
    file.seek(SeekFrom::Start(tail_start))
        .and_then(|_| file.read_exact(&mut tail))
        .map_err(|error| io_to_parse_err(path, error))?;

    Ok(FileProbe { head, tail })
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
/// - Cursor exists and `current_size < cursor.size` → the file shrank since
///   the last call. Covers both strong truncation (`< byte_pos`) and the
///   subtler partial-trailing-line case where the cursor was parked at
///   `byte_pos` and the file then shrank to a value between `byte_pos` and
///   the previous `size` (the bytes between were rewritten in a same-or-
///   smaller atomic write - resuming at `byte_pos` would read stale
///   overlap). Read from 0.
/// - Cursor exists, `current_size == cursor.size`, and `mtime != cursor.mtime`
///   → same-size atomic rewrite; read from 0.
/// - Otherwise → resume from `cursor.byte_pos`. If `current_size == cursor.byte_pos`,
///   the caller will read 0 bytes; the cursor is just refreshed.
fn next_start_offset(
    cursor: Option<&FileCursor>,
    current_size: u64,
    current_mtime: SystemTime,
) -> u64 {
    if requires_replacement(cursor, current_size, current_mtime) {
        0
    } else {
        cursor.map_or(0, |cursor| cursor.byte_pos)
    }
}

fn requires_replacement(
    cursor: Option<&FileCursor>,
    current_size: u64,
    current_mtime: SystemTime,
) -> bool {
    let Some(cursor) = cursor else { return true };
    current_size < cursor.size || (current_size == cursor.size && current_mtime != cursor.mtime)
}

fn identity_changed(cursor: Option<&FileCursor>, current: Option<FileIdentity>) -> bool {
    matches!((cursor.and_then(|cursor| cursor.identity), current), (Some(previous), Some(current)) if previous != current)
}

#[cfg(unix)]
fn file_identity(_file: &fs::File, metadata: &fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    Some(FileIdentity(
        (u128::from(metadata.dev()) << 64) | u128::from(metadata.ino()),
    ))
}

#[cfg(windows)]
fn file_identity(file: &fs::File, _metadata: &fs::Metadata) -> Option<FileIdentity> {
    use std::ffi::c_void;
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle as _;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `file` owns a valid open Windows handle, `information` points to
    // writable storage with the exact BY_HANDLE_FILE_INFORMATION C layout, and
    // the value is read only when the system call reports success.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) != 0 };
    if !succeeded {
        return None;
    }
    // SAFETY: the successful call initialized the complete output structure.
    let information = unsafe { information.assume_init() };
    let file_index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    Some(FileIdentity(
        (u128::from(information.volume_serial_number) << 64) | u128::from(file_index),
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &fs::File, _metadata: &fs::Metadata) -> Option<FileIdentity> {
    None
}

/// Byte length of the longest `\n`-terminated prefix of `buf`.
/// Used both for parsing and for advancing the byte cursor.
fn complete_prefix_len(buf: impl AsRef<[u8]>) -> usize {
    match buf.as_ref().iter().rposition(|byte| *byte == b'\n') {
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
            identity: None,
            probe: FileProbe::default(),
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
    fn shrink_between_byte_pos_and_size_resets_to_zero() {
        // Cursor parked before a partial trailing line: byte_pos=100 (end of
        // last complete line), size=120 (the 20-byte partial line). If the
        // file shrinks to 110 - still >= byte_pos but < size - the bytes
        // between 100 and 120 may have been rewritten. Resuming at 100 would
        // re-read overlap that the writer no longer intends; must reset.
        let c = cursor(100, 120, 100);
        assert_eq!(next_start_offset(Some(&c), 110, mtime_at(150)), 0);
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
        assert!(events.is_replacement());
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
        assert!(!second.is_replacement());
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
    fn bad_complete_line_is_skipped_and_cursor_advances() {
        // Regression: a single malformed *complete* line used to `?`-abort the
        // whole read, leaving the cursor parked in front of it so every later
        // read re-hit the same line and stalled all future appends for the file.
        // Now the bad line is skipped, the good events ingest, and the cursor
        // advances past the whole complete prefix.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        // A schema-drift line: assistant + usage but no top-level timestamp.
        content.push_str(
            r#"{"type":"assistant","message":{"model":"m","usage":{"input_tokens":9,"output_tokens":9}}}"#,
        );
        content.push('\n');
        write_assistant_line(&mut content, "msg_b", "req_2", 200);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let first = p.read_incremental(&path).unwrap();
        assert_eq!(first.len(), 2, "both good lines survive the bad one");
        assert_eq!(
            p.cursor_for(&path).unwrap().byte_pos,
            content.len() as u64,
            "cursor advanced past the whole prefix, not parked on the bad line"
        );

        // Forward progress: a later append is still picked up (no stall).
        write_assistant_line(&mut content, "msg_c", "req_3", 300);
        std::fs::write(&path, &content).unwrap();
        let second = p.read_incremental(&path).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].message_id.as_deref(), Some("msg_c"));
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
    fn partial_utf8_codepoint_after_complete_prefix_parks_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut first_line = String::new();
        write_assistant_line(&mut first_line, "msg_a", "req_1", 100);

        let mut second_line = String::new();
        write_assistant_line(&mut second_line, "msg_b", "req_2", 200);
        let model = second_line.find("\"model\":\"m").unwrap() + "\"model\":\"m".len();
        second_line.insert(model, 'é');
        let split = second_line.find('é').unwrap() + 1;

        let mut partial = first_line.as_bytes().to_vec();
        partial.extend_from_slice(&second_line.as_bytes()[..split]);
        std::fs::write(&path, &partial).unwrap();

        let mut parser = IncrementalParser::new();
        let first = parser.read_incremental(&path).unwrap();
        assert_eq!(first.events().len(), 1);
        assert_eq!(
            parser.cursor_for(&path).unwrap().byte_pos,
            first_line.len() as u64,
            "cursor must park before the incomplete UTF-8 line"
        );

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write as _;
        file.write_all(&second_line.as_bytes()[split..]).unwrap();

        let second = parser.read_incremental(&path).unwrap();
        assert_eq!(second.events().len(), 1);
        assert_eq!(second.events()[0].message_id.as_deref(), Some("msg_b"));
    }

    #[test]
    fn invalid_utf8_complete_line_is_skipped_without_stalling_later_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut first = String::new();
        write_assistant_line(&mut first, "msg_a", "req_1", 100);
        let mut last = String::new();
        write_assistant_line(&mut last, "msg_b", "req_2", 200);
        let mut content = first.into_bytes();
        content.extend_from_slice(&[0xff, b'\n']);
        content.extend_from_slice(last.as_bytes());
        std::fs::write(&path, &content).unwrap();

        let mut parser = IncrementalParser::new();
        let events = parser.read_incremental(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].message_id.as_deref(), Some("msg_a"));
        assert_eq!(events[1].message_id.as_deref(), Some("msg_b"));
        assert_eq!(
            parser.cursor_for(&path).unwrap().byte_pos,
            content.len() as u64
        );
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
        assert!(second.is_replacement());
        assert_eq!(second.len(), 1, "truncation should restart from byte 0");
        assert_eq!(second[0].message_id.as_deref(), Some("msg_x"));
    }

    #[test]
    fn larger_atomic_replacement_is_not_misclassified_as_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut original = String::new();
        write_assistant_line(&mut original, "msg_a", "req_1", 100);
        std::fs::write(&path, &original).unwrap();

        let mut parser = IncrementalParser::new();
        assert!(parser.read_incremental(&path).unwrap().is_replacement());

        let mut replacement = String::new();
        write_assistant_line(&mut replacement, "msg_b", "req_2", 200);
        write_assistant_line(&mut replacement, "msg_c", "req_3", 300);
        assert!(replacement.len() > original.len());
        replace_file(&path, replacement.as_bytes());

        let second = parser.read_incremental(&path).unwrap();
        assert!(second.is_replacement());
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].message_id.as_deref(), Some("msg_b"));
    }

    #[test]
    fn growing_in_place_rewrite_replaces_the_old_contribution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut original = String::new();
        write_assistant_line(&mut original, "msg_old", "req_old", 100);
        std::fs::write(&path, &original).unwrap();

        let mut parser = IncrementalParser::new();
        let first = parser.read_incremental(&path).unwrap();
        assert_eq!(first[0].message_id.as_deref(), Some("msg_old"));
        let original_identity = parser.cursor_for(&path).unwrap().identity;

        let mut rewritten = String::new();
        write_assistant_line(&mut rewritten, "msg_new_a", "req_new_a", 200);
        write_assistant_line(&mut rewritten, "msg_new_b", "req_new_b", 300);
        assert!(rewritten.len() > original.len());
        // `fs::write` truncates and rewrites the existing file handle rather
        // than publishing a renamed replacement, so its physical identity is
        // stable while both the committed prefix and total size change.
        std::fs::write(&path, &rewritten).unwrap();

        let second = parser.read_incremental(&path).unwrap();
        assert_eq!(
            parser.cursor_for(&path).unwrap().identity,
            original_identity
        );
        assert!(second.is_replacement());
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].message_id.as_deref(), Some("msg_new_a"));
        assert_eq!(second[1].message_id.as_deref(), Some("msg_new_b"));
    }

    fn replace_file(path: &Path, bytes: &[u8]) {
        let replacement = path.with_extension("replacement");
        std::fs::write(&replacement, bytes).unwrap();
        #[cfg(windows)]
        std::fs::remove_file(path).unwrap();
        std::fs::rename(replacement, path).unwrap();
    }

    #[test]
    fn partial_trailing_line_then_shrink_rewrite_re_reads_from_zero() {
        // The integration version of `shrink_between_byte_pos_and_size_resets_to_zero`.
        // 1. Write one complete line plus a partial trailing line. After the
        //    first read, the cursor parks at byte_pos = end-of-complete-line,
        //    size = full file (including the partial bytes).
        // 2. Rewrite the file to a size strictly between byte_pos and the
        //    previous size - emulating a same-process recovery where the
        //    writer truncated and re-appended a *different* partial+complete
        //    line. The new bytes overlap the cursor's recorded byte_pos.
        // 3. The next read MUST start at byte 0; resuming at byte_pos would
        //    consume rewritten bytes as if they were fresh appends.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut content = String::new();
        write_assistant_line(&mut content, "msg_a", "req_1", 100);
        let partial_prefix = r#"{"type":"assistant","timestamp":"2026-05-06T14:29:00"#;
        content.push_str(partial_prefix);
        std::fs::write(&path, &content).unwrap();

        let mut p = IncrementalParser::new();
        let first = p.read_incremental(&path).unwrap();
        assert_eq!(first.len(), 1);
        let cursor_after_first = p.cursor_for(&path).unwrap().clone();
        assert!(
            cursor_after_first.byte_pos < cursor_after_first.size,
            "precondition: cursor parked before the partial trailing line"
        );

        // Rewrite to a size between byte_pos and the original size - the
        // shrink the pre-fix code would have missed (current_size >= byte_pos).
        let mut rewritten = String::new();
        write_assistant_line(&mut rewritten, "msg_b", "req_2", 222);
        assert!(
            (rewritten.len() as u64) >= cursor_after_first.byte_pos
                && (rewritten.len() as u64) < cursor_after_first.size,
            "test fixture invariant: rewritten size must land in (byte_pos, size)"
        );
        std::fs::write(&path, &rewritten).unwrap();

        let second = p.read_incremental(&path).unwrap();
        assert_eq!(
            second.len(),
            1,
            "shrink-into-byte_pos must restart from zero"
        );
        assert_eq!(second[0].message_id.as_deref(), Some("msg_b"));
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
