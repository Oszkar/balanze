//! `statusline` subcommand: Claude Code's statusLine command. Reads the
//! statusLine JSON on stdin, prints a one-line status, and atomically writes
//! the snapshot file the watcher reads.

use anyhow::Result;
use std::io::Write;

use crate::format::micro_usd_to_display_dollars;

pub(crate) fn cmd_statusline() -> Result<()> {
    use std::io::Read as _;
    let mut stdout = std::io::stdout().lock();
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        let _ = writeln!(stdout, "bal (statusline: stdin unreadable)");
        return Ok(());
    }
    // Parse once — both the human formatter and the snapshot writer need the
    // result. Parse error → print the error line and skip the write (no good
    // payload to persist for the watcher).
    let snap = match claude_statusline::parse(&buf) {
        Ok(s) => s,
        Err(_) => {
            let _ = writeln!(stdout, "bal (statusline parse error)");
            return Ok(());
        }
    };
    let _ = writeln!(stdout, "{}", format_statusline_from_snapshot(&snap));
    // Independent error handling, not independent timing: the stdout write
    // is synchronous so backpressure DOES delay the snapshot write, but
    // any `writeln!` error is discarded via `let _ =` so we still attempt
    // the snapshot write afterwards. Conversely the human line is already
    // flushed before write_statusline_snapshot runs, so a snapshot-write
    // failure can't suppress it. Together: each side's failures are
    // isolated from the other side's output.
    write_statusline_snapshot(&snap);
    Ok(())
}

/// Formats a parsed [`claude_statusline::StatuslineSnapshot`] into a terse
/// one-liner for Claude Code's statusLine display. A minimal honest line;
/// rich/configurable formatting and feeding the live Snapshot come later.
fn format_statusline_from_snapshot(snap: &claude_statusline::StatuslineSnapshot) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(rl) = &snap.rate_limits {
        // {:.0}: a statusline is a glance — sub-1% truncation is acceptable
        // here. compact_anthropic_quota uses {:.1} to avoid the "0%" == "no
        // usage" ambiguity in the full terminal view; that concern does not
        // apply to a terse one-liner. Intentional inconsistency — do not
        // "align" these without re-reading both rationales.
        if let Some(w) = &rl.five_hour {
            parts.push(format!("5h {:.0}%", w.used_percent));
        }
        if let Some(w) = &rl.seven_day {
            parts.push(format!("7d {:.0}%", w.used_percent));
        }
    }
    if let Some(c) = snap.session_cost_micro_usd {
        // `sess-est`, not `sess`: this is a Claude-side session estimate
        // (claude_statusline/types.rs:22) — a distinct cost tier from the
        // JSONL list-price estimate and the real `extra_usage` overage.
        // The qualifier mirrors compact_anthropic_quota's `est-leverage`
        // discipline so a statusline glance can't be mistaken for billed $.
        parts.push(format!("sess-est {}", micro_usd_to_display_dollars(c)));
    }
    if parts.is_empty() {
        "bal (no rate-limit data yet)".to_string()
    } else {
        format!("bal {}", parts.join(" · "))
    }
}

/// Pure: payload string → one status-line string. Thin wrapper around
/// [`format_statusline_from_snapshot`] retained for the existing test suite
/// which supplies a raw JSON string. New callers should parse first and call
/// the typed helper directly.
#[cfg(test)]
fn format_statusline(payload: &str) -> String {
    let snap = match claude_statusline::parse(payload) {
        Ok(s) => s,
        Err(_) => return "bal (statusline parse error)".to_string(),
    };
    format_statusline_from_snapshot(&snap)
}

/// Writes the parsed statusline snapshot to `<data_dir>/statusline.snapshot.json`
/// — where `<data_dir>` is `directories::ProjectDirs.data_dir()`, which
/// already includes the per-OS Balanze subpath — for the watcher
/// to notify-watch.
///
/// Write failures log at `warn!` and are swallowed — Claude Code's statusLine
/// call must not fail because Balanze's IPC file failed (which would cause the
/// user's statusLine to disappear from their terminal).
fn write_statusline_snapshot(snap: &claude_statusline::StatuslineSnapshot) {
    let Some(path) = statusline_snapshot_path() else {
        tracing::warn!("statusline: could not resolve data dir; skipping snapshot write");
        return;
    };
    let envelope = claude_statusline::StatuslineFilePayload::new(snap.clone(), chrono::Utc::now());
    if let Err(e) = claude_statusline::atomic_write_snapshot(&path, &envelope) {
        tracing::warn!("statusline: snapshot write failed: {e}");
    }
}

/// Resolves the path to the watcher IPC file.
///
/// When `BALANZE_DATA_DIR_OVERRIDE` is set, the snapshot file lands at
/// `<override>/statusline.snapshot.json` — intended for tests only.
/// In normal operation, the path follows `directories::ProjectDirs` so all
/// persistent locations go through the same crate (AGENTS.md §2.1 convention).
fn statusline_snapshot_path() -> Option<std::path::PathBuf> {
    if let Ok(env_path) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(std::path::PathBuf::from(env_path).join("statusline.snapshot.json"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.data_dir().join("statusline.snapshot.json"))
}

#[cfg(test)]
mod statusline_tests {
    use super::format_statusline;

    /// Process-wide lock for tests that mutate a shared environment variable.
    /// Cargo test parallelizes per-crate by default; two tests that both
    /// `set_var(BALANZE_DATA_DIR_OVERRIDE, …)` with different values would
    /// otherwise race and read each other's values. The lock serializes them.
    /// (We avoid adding `serial_test` as a dev-dep just for this one
    /// crate-internal need.)
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: acquires the process-wide [`ENV_LOCK`], sets the env var
    /// to `value`, and on `Drop` (including panic unwind) restores the prior
    /// value before releasing the lock. The lock is held for the test's full
    /// duration so no concurrent test can observe a half-set state.
    ///
    /// Field-drop order is declaration order, and `Drop::drop` runs before
    /// any field drops — so the restore happens first, then `_lock` releases
    /// last. A poisoned lock (from a panicked predecessor) is recovered via
    /// `into_inner()`: we still want a consistent env-var state for this
    /// test, and the predecessor's `Drop` has already restored its part.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var(key).ok();
            // SAFETY: ENV_LOCK (held for this guard's whole lifetime) serializes
            // every env-touching statusline test, so no concurrent reader races
            // this write. set_var is unsafe as of edition 2024.
            unsafe { std::env::set_var(key, value) };
            Self { key, prev, _lock }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `EnvGuard::set` — ENV_LOCK is still held here, so the
            // restore is serialized against all other env-touching tests.
            // set_var/remove_var are unsafe as of edition 2024.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn formats_full_payload() {
        let p = r#"{"rate_limits":{"five_hour":{"used_percentage":13.0,"resets_at":1747650600},"seven_day":{"used_percentage":44.0,"resets_at":1747915200}},"cost":{"total_cost_usd":12.5}}"#;
        assert_eq!(
            format_statusline(p),
            "bal 5h 13% · 7d 44% · sess-est $12.50"
        );
    }
    #[test]
    fn formats_no_rate_limits() {
        assert_eq!(
            format_statusline(r#"{"cost":{"total_cost_usd":2.0}}"#),
            "bal sess-est $2.00"
        );
    }
    #[test]
    fn formats_empty_payload() {
        assert_eq!(format_statusline("{}"), "bal (no rate-limit data yet)");
    }
    #[test]
    fn parse_error_is_nonempty_fallback_not_panic() {
        assert_eq!(
            format_statusline("not json"),
            "bal (statusline parse error)"
        );
    }
    #[test]
    fn formats_only_seven_day() {
        let p = r#"{"rate_limits":{"seven_day":{"used_percentage":72.0,"resets_at":1747915200}}}"#;
        assert_eq!(format_statusline(p), "bal 7d 72%");
    }

    #[test]
    fn statusline_snapshot_path_honors_env_override() {
        let _guard = EnvGuard::set("BALANZE_DATA_DIR_OVERRIDE", "/tmp/balanze-test");
        let p = super::statusline_snapshot_path().unwrap();
        assert_eq!(
            p,
            std::path::PathBuf::from("/tmp/balanze-test/statusline.snapshot.json")
        );
    }

    #[test]
    fn write_statusline_snapshot_lands_at_data_dir_override() {
        use claude_statusline::{SCHEMA_VERSION, StatuslineSnapshot, read_snapshot};

        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("BALANZE_DATA_DIR_OVERRIDE", dir.path());

        let snap = StatuslineSnapshot {
            rate_limits: None,
            session_cost_micro_usd: Some(3_420_000),
            claude_code_version: Some("v2.1.144".to_string()),
        };
        super::write_statusline_snapshot(&snap);

        let written = read_snapshot(&dir.path().join("statusline.snapshot.json")).unwrap();
        assert_eq!(written.schema_version, SCHEMA_VERSION);
        assert_eq!(written.payload.session_cost_micro_usd, Some(3_420_000));
    }
}
