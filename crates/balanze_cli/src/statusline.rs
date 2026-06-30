//! `statusline` subcommand: Claude Code's statusLine command. Reads the
//! statusLine JSON on stdin, prints the configured multi-line status, and
//! atomically writes the snapshot file the watcher reads.

use anyhow::Result;
use std::io::Write;

pub(crate) fn cmd_statusline() -> Result<()> {
    use std::io::Read as _;
    let mut stdout = std::io::stdout().lock();
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        let _ = writeln!(stdout, "bal (statusline: stdin unreadable)");
        return Ok(());
    }
    // Parse once - both the renderer and the snapshot writer need the
    // result. Parse error -> print the error line and skip the write (no good
    // payload to persist for the watcher).
    let snap = match claude_statusline::parse(&buf) {
        Ok(s) => s,
        Err(_) => {
            let _ = writeln!(stdout, "bal (statusline parse error)");
            return Ok(());
        }
    };
    let _ = writeln!(stdout, "{}", render_line(&snap));
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

/// How fresh `snapshot.json` must be for its cross-provider cells to render
/// without a staleness marker. The host rewrites the file on every coordinator
/// update (safety-poll floor 60s), so a running host stays well inside this.
const SNAPSHOT_FRESHNESS_SECS: i64 = 120;

/// Read the host-written `snapshot.json` and map its Codex/OpenAI cells into the
/// renderer's cross-provider input. `None` (Claude-only) when the file is
/// absent, unreadable, or schema-drifted - PR2 does ZERO network here; the
/// self-compose fallback is PR3.
fn statusline_cross_provider() -> Option<statusline_render::CrossProvider> {
    let path = state_coordinator::snapshot_file_path()?;
    match state_coordinator::read_snapshot_file(&path) {
        Ok(payload) => Some(cross_from_payload(&payload, chrono::Utc::now())),
        // FileMissing is the normal "host not running" case - stay silent.
        Err(state_coordinator::SnapshotFileError::FileMissing { .. }) => None,
        // ParseError / SchemaDrift / Io are genuinely unexpected; surface them
        // at debug (BALANZE_LOG=debug) without any production noise.
        Err(e) => {
            tracing::debug!(
                "statusline: cross-provider snapshot unreadable, falling back to Claude-only: {e}"
            );
            None
        }
    }
}

/// Pure map: snapshot-file payload -> `CrossProvider`. `stale` when the payload
/// is older than the freshness window (stale-but-known data is still shown with
/// a marker rather than hidden - the project's stale-with-indicator rule).
fn cross_from_payload(
    payload: &state_coordinator::SnapshotFilePayload,
    now: chrono::DateTime<chrono::Utc>,
) -> statusline_render::CrossProvider {
    let snap = &payload.snapshot;
    let age = now.signed_duration_since(payload.captured_at).num_seconds();
    statusline_render::CrossProvider {
        codex_used_percent: snap
            .codex_quota
            .as_ref()
            .map(|q| q.primary.used_percent as f32),
        openai_cost_micro_usd: snap.openai.as_ref().map(|c| c.total_micro_usd),
        stale: age > SNAPSHOT_FRESHNESS_SECS,
    }
}

/// Render the configured statusline for `snap`, reading the user's settings.
/// Settings load failure falls back to the curated default (the statusline must
/// never fail to render). Color is gated on `NO_COLOR` only - Claude Code
/// captures stdout (not a TTY) and renders ANSI, so TTY detection would wrongly
/// strip color.
fn render_line(snap: &claude_statusline::StatuslineSnapshot) -> String {
    let settings = settings::load().unwrap_or_default();
    let color = std::env::var_os("NO_COLOR").is_none();
    let cross = statusline_cross_provider();
    render_with(snap, &settings.statusline, color, cross.as_ref())
}

/// Testable core: render `snap` against an explicit config. Kept separate from
/// `render_line` so tests do not depend on the developer's real settings.json.
fn render_with(
    snap: &claude_statusline::StatuslineSnapshot,
    config: &settings::StatuslineConfig,
    color: bool,
    cross: Option<&statusline_render::CrossProvider>,
) -> String {
    statusline_render::render(&statusline_render::RenderInput {
        snapshot: snap,
        cross,
        config,
        now: chrono::Utc::now(),
        color,
    })
}

/// Writes the parsed statusline snapshot to `<data_dir>/statusline.snapshot.json`
/// (where `<data_dir>` is `directories::ProjectDirs.data_dir()`, which already
/// includes the per-OS Balanze subpath) for the watcher to notify-watch.
///
/// Write failures log at `warn!` and are swallowed - Claude Code's statusLine
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
/// `<override>/statusline.snapshot.json` - intended for tests only.
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
    /// Process-wide lock for tests that mutate a shared environment variable.
    /// Cargo test parallelizes per-crate by default; two tests that both
    /// `set_var(BALANZE_DATA_DIR_OVERRIDE, ...)` with different values would
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
    /// any field drops - so the restore happens first, then `_lock` releases
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
            // SAFETY: see `EnvGuard::set` - ENV_LOCK is still held here, so the
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
    fn render_with_default_config_contains_known_segments() {
        let snap = claude_statusline::parse(
            r#"{"rate_limits":{"five_hour":{"used_percentage":82,"resets_at":4102444800}},"cost":{"total_cost_usd":2.5},"model":{"display_name":"Opus"}}"#,
        )
        .unwrap();
        // color=false for a deterministic, escape-free assertion.
        let out = super::render_with(&snap, &settings::StatuslineConfig::default(), false, None);
        assert!(out.contains("🤖 Opus"), "{out}");
        assert!(out.contains("5h 82%"), "{out}");
        assert!(out.contains("💰 ~$2.50"), "{out}");
    }

    #[test]
    fn cross_from_payload_maps_cells_and_freshness() {
        use chrono::TimeZone as _;
        let now = chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap();
        let mut s = state_coordinator::Snapshot::empty(now);
        s.codex_quota = Some(codex_local::types::CodexQuotaSnapshot {
            observed_at: now,
            session_id: "s".into(),
            primary: codex_local::types::RateLimitWindow {
                used_percent: 6.0,
                window_duration_minutes: 10_080,
                resets_at: now,
            },
            secondary: None,
            plan_type: "go".into(),
            rate_limit_reached: false,
        });
        s.openai = Some(openai_client::OpenAiCosts {
            start_time: now,
            end_time: now,
            total_micro_usd: 4_200_000,
            by_line_item: vec![],
            truncated: false,
            fetched_at: now,
        });
        let fresh = state_coordinator::SnapshotFilePayload::new(s.clone(), now);
        let c = super::cross_from_payload(&fresh, now);
        assert_eq!(c.codex_used_percent, Some(6.0));
        assert_eq!(c.openai_cost_micro_usd, Some(4_200_000));
        assert!(!c.stale, "fresh payload is not stale");
        let stale_payload =
            state_coordinator::SnapshotFilePayload::new(s, now - chrono::Duration::seconds(200));
        assert!(super::cross_from_payload(&stale_payload, now).stale);
    }

    #[test]
    fn cross_from_empty_snapshot_has_no_cells() {
        use chrono::TimeZone as _;
        let now = chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap();
        let payload = state_coordinator::SnapshotFilePayload::new(
            state_coordinator::Snapshot::empty(now),
            now,
        );
        let c = super::cross_from_payload(&payload, now);
        assert!(c.codex_used_percent.is_none());
        assert!(c.openai_cost_micro_usd.is_none());
    }

    #[test]
    fn cross_renders_codex_and_openai_segments() {
        let snap = claude_statusline::parse(r#"{"model":{"display_name":"Opus"}}"#).unwrap();
        let cross = statusline_render::CrossProvider {
            codex_used_percent: Some(6.0),
            openai_cost_micro_usd: Some(4_200_000),
            stale: false,
        };
        let out = super::render_with(
            &snap,
            &settings::StatuslineConfig::default(),
            false,
            Some(&cross),
        );
        assert!(out.contains("Codex 6%"), "{out}");
        assert!(out.contains("OpenAI $4.20"), "{out}");
    }

    #[test]
    fn cross_provider_returns_none_when_snapshot_absent() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set("BALANZE_DATA_DIR_OVERRIDE", dir.path());
        // snapshot.json does not exist in `dir` (host not running) -> Claude-only.
        assert!(super::statusline_cross_provider().is_none());
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
            model_display_name: None,
            context_used_percent: None,
        };
        super::write_statusline_snapshot(&snap);

        let written = read_snapshot(&dir.path().join("statusline.snapshot.json")).unwrap();
        assert_eq!(written.schema_version, SCHEMA_VERSION);
        assert_eq!(written.payload.session_cost_micro_usd, Some(3_420_000));
    }
}
