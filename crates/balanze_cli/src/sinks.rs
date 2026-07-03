//! Stdout and JSONL sinks for `--watch` mode.
//!
//! The `Sink` trait is synchronous; the coordinator's actor calls these from
//! its tokio task. Each sink owns whatever state it needs - no shared mutable
//! state.
//!
//! # Sink choices
//!
//! * [`StdoutSink`] - the default. On a TTY it reprints the compact 4-quadrant
//!   view with an ANSI clear-screen before each frame. On non-TTY (pipes, log
//!   files) it prepends `---` so output is parseable. Both paths are debounced
//!   at 200 ms.
//!
//! * [`JsonlSink`] - activated by `--watch --json`. Emits one JSON object per
//!   line with no debounce (every event is a discrete data point for the
//!   consumer).
//!
//! # Platform / terminal assumptions
//!
//! `StdoutSink`'s TTY path emits `\x1b[2J\x1b[H` (ANSI clear-screen + cursor-
//! home) and relies on the terminal to interpret it. This works on:
//!
//! * macOS Terminal / iTerm2
//! * Linux on any modern terminal emulator
//! * Windows 11 in **Windows Terminal** and **PowerShell 7+** (both enable
//!   Virtual Terminal Processing by default - see `ENABLE_VIRTUAL_TERMINAL_PROCESSING`)
//!
//! It does NOT work in legacy Windows `cmd.exe` without `EnableVirtualTerminalProcessing`
//! turned on first; in that case the user will see literal `␛[2J␛[H` characters
//! and the redraw effect breaks. Since the distribution story is
//! source-only (the audience is power users on Win 11 / Windows Terminal) we
//! accept that limitation rather than pull in `crossterm` for a UI-only
//! concern. If a future user hits this, the fix is a one-time
//! `windows-sys`-backed `SetConsoleMode` call in `StdoutSink::new`.
//!
//! # Broken-pipe handling
//!
//! `StdoutSink` tracks a `broken_pipe` flag: once a write to stdout returns
//! `ErrorKind::BrokenPipe` (e.g., `balanze-cli --watch | head -n 5` closes
//! the pipe after 5 frames), the sink stops attempting further writes - but
//! the coordinator's loop keeps running. This is a deliberate trade-off:
//! returning an error from the sync `Sink::on_snapshot` boundary would require
//! a trait extension, while the broken-pipe condition itself isn't fatal -
//! the coordinator stays alive and `Ctrl-C` still exits cleanly. The JSONL
//! path writes directly to stdout and drops write errors at the sync boundary
//! for the same reason.

use std::io::{Stderr, Write, stderr};
use std::time::{Duration, Instant};

use state_coordinator::{Sink, Snapshot, Source};

use crate::json_output;
use crate::render::write_compact;

/// How close two frames must arrive before the second is dropped.
///
/// Trade-off: a burst's **last** frame is dropped if no further event arrives
/// within DEBOUNCE of the previously-painted frame. This is acceptable for
/// the watcher's cadence (5-min OAuth poll baseline + sporadic notify events
/// ensure another event will come soon) and avoids the complexity of a
/// separate trailing-edge timer task.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// ANSI escape: clear entire screen (`\x1b[2J`) + move cursor to top-left
/// (`\x1b[H`). Together they produce the in-place-redraw effect the TTY
/// path relies on. See module doc for terminal-compat caveats.
const ANSI_CLEAR_HOME: &[u8] = b"\x1b[2J\x1b[H";

// ─────────────────────────────────────────────────────────────────────────────
// StdoutSink
// ─────────────────────────────────────────────────────────────────────────────

/// A [`Sink`] that redraws the compact 4-quadrant view on stdout.
///
/// * **TTY** - emits `\x1b[2J\x1b[H` (ANSI clear-screen + cursor-home) before
///   each frame, giving an in-place refresh effect.
/// * **Non-TTY** - prepends `---\n` before each frame so the stream is
///   parseable as a series of separator-delimited blocks.
///
/// Frames arriving sooner than [`DEBOUNCE`] after the previous painted frame
/// are silently dropped (see the trade-off comment on `DEBOUNCE`).
pub struct StdoutSink {
    out: Box<dyn Write + Send>,
    err: Stderr,
    is_tty: bool,
    last_render: Option<Instant>,
    /// Set to true on the first write that returns `ErrorKind::BrokenPipe`.
    /// Subsequent `on_snapshot` calls become no-ops - see module doc.
    broken_pipe: bool,
}

impl StdoutSink {
    /// Construct a `StdoutSink` wired to the real stdout + stderr. TTY status
    /// is detected via `std::io::IsTerminal`.
    pub fn new() -> Self {
        use std::io::IsTerminal;
        let is_tty = std::io::stdout().is_terminal();
        Self {
            out: Box::new(std::io::stdout()),
            err: stderr(),
            is_tty,
            last_render: None,
            broken_pipe: false,
        }
    }

    /// Construct a `StdoutSink` with a caller-provided writer and explicit
    /// TTY flag. Used by unit tests to capture output without spawning the
    /// watcher or touching real stdout.
    #[cfg(test)]
    pub(crate) fn new_with(out: Box<dyn Write + Send>, is_tty: bool) -> Self {
        Self {
            out,
            err: stderr(),
            is_tty,
            last_render: None,
            broken_pipe: false,
        }
    }

    /// Write helper that records broken-pipe state. Returns true if the
    /// write succeeded, false if a broken-pipe error was observed (in
    /// which case `self.broken_pipe` is set and no further writes are
    /// attempted by `on_snapshot` for this sink's lifetime).
    fn write_or_record_broken_pipe(&mut self, buf: &[u8]) -> bool {
        match self.out.write_all(buf) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                self.broken_pipe = true;
                false
            }
            Err(_) => false, // other errors: silent, but don't latch
        }
    }
}

impl Sink for StdoutSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        if self.broken_pipe {
            return;
        }
        let now = Instant::now();
        if let Some(prev) = self.last_render {
            if now.duration_since(prev) < DEBOUNCE {
                return;
            }
        }
        // NB: `last_render` is set AFTER a successful write below, not
        // here. If the write fails with a non-broken-pipe I/O error
        // (rare - e.g., transient stdout EAGAIN), the next snapshot
        // arriving within DEBOUNCE should still attempt a paint rather
        // than being dropped on top of the missed one. Broken-pipe
        // failures latch via `self.broken_pipe` and become permanent.

        if self.is_tty {
            if !self.write_or_record_broken_pipe(ANSI_CLEAR_HOME) {
                return;
            }
        } else if !self.write_or_record_broken_pipe(b"---\n") {
            return;
        }
        // `write_compact` returns io::Result; capture broken-pipe via match.
        if let Err(e) = write_compact(snapshot, &mut self.out) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                self.broken_pipe = true;
            }
            return;
        }
        let _ = self.out.flush();
        // Successful paint - record the timestamp so subsequent calls
        // within DEBOUNCE are dropped.
        self.last_render = Some(now);
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        // Degraded indicators go to stderr so they don't interleave with the
        // stdout TUI redraw (especially on TTY where we clear the screen).
        let _ = writeln!(self.err, "[degraded] {source:?}: {error}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// JsonlSink
// ─────────────────────────────────────────────────────────────────────────────

/// A [`Sink`] that emits one JSON object per line on stdout.
///
/// Suitable for piping into `jq` or any line-delimited JSON consumer. Every
/// `on_snapshot` event produces exactly one line; there is no debounce because
/// each event is a discrete data point.
///
/// `on_degraded` is intentionally a no-op: errors ride in the next snapshot's
/// `*_error` slots, and emitting a separate line on degraded events would break
/// the "one JSON object per line" invariant that jq and similar tools rely on.
pub struct JsonlSink {
    out: Box<dyn Write + Send>,
    verbose: bool,
}

impl JsonlSink {
    pub fn new(verbose: bool) -> Self {
        Self {
            out: Box::new(std::io::stdout()),
            verbose,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with(out: Box<dyn Write + Send>, verbose: bool) -> Self {
        Self { out, verbose }
    }
}

impl Sink for JsonlSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        // Use `render_jsonl` (single-line `to_string`), NOT `render`
        // (`to_string_pretty` - multi-line with embedded newlines).
        // `--watch --json` must produce exactly one JSON object per
        // line so `jq` and other line-oriented consumers work.
        match json_output::render_jsonl(snapshot, self.verbose) {
            Ok(line) => {
                if writeln!(self.out, "{line}").is_ok() {
                    let _ = self.out.flush();
                }
            }
            Err(e) => eprintln!("[json render error] {e}"),
        }
    }

    fn on_degraded(&mut self, _source: Source, _error: &str) {
        // Intentional no-op - see module doc.
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use state_coordinator::Snapshot;
    use std::sync::{Arc, Mutex};

    /// A `Write` impl backed by a shared `Vec<u8>`, so the test can read back
    /// what the sink wrote without unwrapping `Box<dyn Write>`.
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn make_sink_and_buf(is_tty: bool) -> (StdoutSink, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = TestWriter(Arc::clone(&buf));
        let sink = StdoutSink::new_with(Box::new(writer), is_tty);
        (sink, buf)
    }

    fn fixture_snapshot() -> Snapshot {
        Snapshot::empty(Utc::now())
    }

    fn identifiable_snapshot() -> Snapshot {
        let now = Utc::now();
        let mut snap = Snapshot::empty(now);
        snap.claude_oauth = Some(anthropic_oauth::ClaudeOAuthSnapshot {
            cadences: Vec::new(),
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: Some("org_test_123".to_string()),
            fetched_at: now,
        });
        snap.codex_quota = Some(codex_local::CodexQuotaSnapshot {
            observed_at: now,
            session_id: "session_test_456".to_string(),
            primary: codex_local::RateLimitWindow {
                used_percent: 12.5,
                window_duration_minutes: 10_080,
                resets_at: now,
            },
            secondary: None,
            plan_type: "go".to_string(),
            rate_limit_reached: false,
        });
        snap
    }

    #[test]
    fn stdout_sink_non_tty_writes_separator_and_compact_view() {
        let (mut sink, buf) = make_sink_and_buf(false);
        sink.on_snapshot(&fixture_snapshot());

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        // Non-TTY path: must start with the separator line.
        assert!(
            output.starts_with("---\n"),
            "expected '---\\n' prefix, got: {:?}",
            &output[..output.len().min(40)]
        );
        // Must also contain the compact header.
        assert!(
            output.contains("=== Balanze status"),
            "compact header missing in: {:?}",
            &output[..output.len().min(120)]
        );
    }

    #[test]
    fn stdout_sink_tty_writes_ansi_clear_then_compact() {
        let (mut sink, buf) = make_sink_and_buf(true);
        sink.on_snapshot(&fixture_snapshot());

        let output = buf.lock().unwrap().clone();
        // TTY path: must start with the ANSI clear sequence.
        assert!(
            output.starts_with(ANSI_CLEAR_HOME),
            "expected ANSI clear prefix"
        );
        let text = String::from_utf8(output).unwrap();
        assert!(
            text.contains("=== Balanze status"),
            "compact header missing"
        );
    }

    #[test]
    fn jsonl_sink_verbose_reveals_identifiers() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = TestWriter(Arc::clone(&buf));
        let mut sink = JsonlSink::new_with(Box::new(writer), true);

        sink.on_snapshot(&identifiable_snapshot());

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains(r#""org_uuid":"org_test_123""#));
        assert!(output.contains(r#""session_id":"session_test_456""#));
        assert!(output.ends_with('\n'));
    }

    /// `Write` impl that returns `BrokenPipe` on every `write` call - used
    /// to verify `StdoutSink` latches into broken-pipe state and stops
    /// trying to write.
    struct BrokenPipeWriter {
        write_calls: Arc<Mutex<u32>>,
    }
    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            *self.write_calls.lock().unwrap() += 1;
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "test: pipe closed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stdout_sink_latches_broken_pipe_and_stops_writing() {
        let write_calls = Arc::new(Mutex::new(0u32));
        let writer = BrokenPipeWriter {
            write_calls: Arc::clone(&write_calls),
        };
        let mut sink = StdoutSink::new_with(Box::new(writer), false);

        sink.on_snapshot(&fixture_snapshot());
        let calls_after_first = *write_calls.lock().unwrap();
        assert!(
            calls_after_first >= 1,
            "first frame should attempt at least one write"
        );

        // Bypass the debounce so we hit the broken_pipe gate, not the
        // last_render gate.
        sink.last_render = None;
        sink.on_snapshot(&fixture_snapshot());
        let calls_after_second = *write_calls.lock().unwrap();
        assert_eq!(
            calls_after_first, calls_after_second,
            "after broken-pipe latch, no further writes should be attempted"
        );
    }

    #[test]
    fn stdout_sink_debounce_drops_frames_within_window() {
        let (mut sink, buf) = make_sink_and_buf(false);

        // First call renders.
        sink.on_snapshot(&fixture_snapshot());
        let len_after_first = buf.lock().unwrap().len();
        assert!(len_after_first > 0, "first frame should have written bytes");

        // Second call immediately after - should be dropped by the debounce.
        sink.on_snapshot(&fixture_snapshot());
        let len_after_second = buf.lock().unwrap().len();
        assert_eq!(
            len_after_first, len_after_second,
            "second frame within debounce window should be a no-op"
        );
    }
}
