//! Stdout and JSONL sinks for `--watch` mode.
//!
//! The `Sink` trait is synchronous; the coordinator's actor calls these from
//! its tokio task. Each sink owns whatever state it needs — no shared mutable
//! state.
//!
//! # Sink choices
//!
//! * [`StdoutSink`] — the default. On a TTY it reprints the compact 4-quadrant
//!   view with an ANSI clear-screen before each frame. On non-TTY (pipes, log
//!   files) it prepends `---` so output is parseable. Both paths are debounced
//!   at 200 ms.
//!
//! * [`JsonlSink`] — activated by `--watch --json`. Emits one JSON object per
//!   line with no debounce (every event is a discrete data point for the
//!   consumer).

use std::io::{stderr, Stderr, Write};
use std::time::{Duration, Instant};

use state_coordinator::{Sink, Snapshot, Source};

use crate::json_output;
use crate::write_compact;

/// How close two frames must arrive before the second is dropped.
///
/// Trade-off: a burst's **last** frame is dropped if no further event arrives
/// within DEBOUNCE of the previously-painted frame. This is acceptable for
/// the watcher's cadence (5-min OAuth poll baseline + sporadic notify events
/// ensure another event will come soon) and avoids the complexity of a
/// separate trailing-edge timer task.
const DEBOUNCE: Duration = Duration::from_millis(200);

// ─────────────────────────────────────────────────────────────────────────────
// StdoutSink
// ─────────────────────────────────────────────────────────────────────────────

/// A [`Sink`] that redraws the compact 4-quadrant view on stdout.
///
/// * **TTY** — emits `\x1b[2J\x1b[H` (ANSI clear-screen + cursor-home) before
///   each frame, giving an in-place refresh effect.
/// * **Non-TTY** — prepends `---\n` before each frame so the stream is
///   parseable as a series of separator-delimited blocks.
///
/// Frames arriving sooner than [`DEBOUNCE`] after the previous painted frame
/// are silently dropped (see the trade-off comment on `DEBOUNCE`).
pub struct StdoutSink {
    out: Box<dyn Write + Send>,
    err: Stderr,
    is_tty: bool,
    last_render: Option<Instant>,
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
        }
    }
}

impl Sink for StdoutSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        let now = Instant::now();
        if let Some(prev) = self.last_render {
            if now.duration_since(prev) < DEBOUNCE {
                return;
            }
        }
        self.last_render = Some(now);

        if self.is_tty {
            // ANSI: clear screen (2J) then move cursor to top-left (H).
            let _ = self.out.write_all(b"\x1b[2J\x1b[H");
        } else {
            let _ = writeln!(self.out, "---");
        }
        let _ = write_compact(snapshot, &mut self.out);
        let _ = self.out.flush();
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
pub struct JsonlSink;

impl Sink for JsonlSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        // verbose=false: machine consumers don't need org_uuid / session_id.
        // A future `--watch --json -v` flag can construct JsonlSink differently
        // and pass verbose=true if identifiers are needed.
        match json_output::render(snapshot, /* verbose */ false) {
            Ok(line) => println!("{line}"),
            Err(e) => eprintln!("[json render error] {e}"),
        }
    }

    fn on_degraded(&mut self, _source: Source, _error: &str) {
        // Intentional no-op — see module doc.
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
            output.starts_with(b"\x1b[2J\x1b[H"),
            "expected ANSI clear prefix"
        );
        let text = String::from_utf8(output).unwrap();
        assert!(
            text.contains("=== Balanze status"),
            "compact header missing"
        );
    }

    #[test]
    fn stdout_sink_debounce_drops_frames_within_window() {
        let (mut sink, buf) = make_sink_and_buf(false);

        // First call renders.
        sink.on_snapshot(&fixture_snapshot());
        let len_after_first = buf.lock().unwrap().len();
        assert!(len_after_first > 0, "first frame should have written bytes");

        // Second call immediately after — should be dropped by the debounce.
        sink.on_snapshot(&fixture_snapshot());
        let len_after_second = buf.lock().unwrap().len();
        assert_eq!(
            len_after_first, len_after_second,
            "second frame within debounce window should be a no-op"
        );
    }
}
