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

/// Resolve cross-provider data (Codex %, OpenAI $) for the statusline.
///
/// Precedence (see PR3 plan): a fresh host-written `snapshot.json` wins (zero
/// network); otherwise self-compose Codex + OpenAI directly (AGENTS.md §3.1: never via the
/// OAuth-touching composer); otherwise fall back to a stale snapshot so the
/// line never blanks; otherwise Claude-only.
fn statusline_cross_provider() -> Option<statusline_render::CrossProvider> {
    let now = chrono::Utc::now();

    // 1. Fresh snapshot wins (zero network).
    let snapshot_cross = read_snapshot_cross(now);
    if let Some(cross) = &snapshot_cross {
        if !cross.openai_stale && !cross.codex_stale {
            return snapshot_cross;
        }
    }

    // 2. Self-compose; 3. stale snapshot is the never-blank fallback.
    pick_cross(self_compose_cross(now), snapshot_cross)
}

/// Choose between a self-composed result and a (possibly stale) snapshot, once
/// the fresh-snapshot short-circuit has been ruled out. Prefer composed data
/// that has at least one cell; otherwise keep the snapshot so the line never
/// blanks. Pure - unit-tested.
fn pick_cross(
    composed: Option<statusline_render::CrossProvider>,
    stale_snapshot: Option<statusline_render::CrossProvider>,
) -> Option<statusline_render::CrossProvider> {
    match composed {
        Some(c) if c.codex_used_percent.is_some() || c.openai_cost_micro_usd.is_some() => Some(c),
        _ => stale_snapshot,
    }
}

/// Run the self-compose path: resolve cache dir + key fingerprint, build a
/// one-shot runtime, and call `statusline_render::self_compose` with the
/// OAuth-free `LiveCrossSources`. `None` if there is no cache dir or the runtime
/// cannot be built; never panics when called from a synchronous context (the
/// `block_on` below would panic if called from within an async runtime).
fn self_compose_cross(
    now: chrono::DateTime<chrono::Utc>,
) -> Option<statusline_render::CrossProvider> {
    let cache_dir = statusline_render::cache::cache_dir_path()?;
    let fingerprint = statusline_render::cache::key_fingerprint(
        crate::sources::resolve_openai_key()
            .ok()
            .flatten()
            .as_deref(),
    );
    // One-shot CLI: a fresh per-turn runtime is acceptable; the OpenAI fetch
    // inside self_compose is cache-gated (300s), so the network is not hit every
    // turn even though the runtime is built every turn.
    let rt = tokio::runtime::Runtime::new().ok()?;
    Some(rt.block_on(statusline_render::self_compose(
        &crate::sources::LiveCrossSources,
        &cache_dir,
        &fingerprint,
        now,
    )))
}

/// Read `snapshot.json` and map it to a `CrossProvider` (cells may be stale).
/// `None` only when the file is absent/unreadable.
fn read_snapshot_cross(
    now: chrono::DateTime<chrono::Utc>,
) -> Option<statusline_render::CrossProvider> {
    let path = state_coordinator::snapshot_file_path()?;
    match state_coordinator::read_snapshot_file(&path) {
        Ok(payload) => Some(cross_from_payload(&payload, now)),
        Err(state_coordinator::SnapshotFileError::FileMissing { .. }) => None,
        Err(e) => {
            tracing::debug!(
                "statusline: cross-provider snapshot unreadable, trying self-compose: {e}"
            );
            None
        }
    }
}

/// Pure map: snapshot-file payload -> `CrossProvider`. Both cells are marked
/// stale when the payload is older than the freshness window (stale-but-known
/// data is still shown with a marker rather than hidden - the project's
/// stale-with-indicator rule).
fn cross_from_payload(
    payload: &state_coordinator::SnapshotFilePayload,
    now: chrono::DateTime<chrono::Utc>,
) -> statusline_render::CrossProvider {
    let snap = &payload.snapshot;
    let age = now.signed_duration_since(payload.captured_at).num_seconds();
    let stale = age > SNAPSHOT_FRESHNESS_SECS;
    statusline_render::CrossProvider {
        codex_used_percent: snap
            .codex_quota
            .as_ref()
            .map(|q| q.primary.used_percent as f32),
        openai_cost_micro_usd: snap.openai.as_ref().map(|c| c.total_micro_usd),
        // Both cells come from one snapshot, so they share its freshness.
        codex_stale: stale,
        openai_stale: stale,
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
        assert!(!c.codex_stale, "fresh payload: codex not stale");
        assert!(!c.openai_stale, "fresh payload: openai not stale");
        let stale_payload =
            state_coordinator::SnapshotFilePayload::new(s, now - chrono::Duration::seconds(200));
        let stale = super::cross_from_payload(&stale_payload, now);
        assert!(stale.codex_stale, "old payload: codex stale");
        assert!(stale.openai_stale, "old payload: openai stale");
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
            codex_stale: false,
            openai_stale: false,
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

    /// Like `EnvGuard` but for multiple env vars under a single `ENV_LOCK`
    /// acquisition. Use when a test needs to set more than one env var
    /// atomically - creating two `EnvGuard`s would deadlock on the same thread.
    struct MultiEnvGuard {
        vars: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl MultiEnvGuard {
        fn set(pairs: &[(&'static str, &std::ffi::OsStr)]) -> Self {
            let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let vars = pairs
                .iter()
                .map(|&(k, v)| {
                    let prev = std::env::var(k).ok();
                    // SAFETY: ENV_LOCK is held for this guard's entire lifetime,
                    // serializing all env-touching tests (see ENV_LOCK comment).
                    unsafe { std::env::set_var(k, v) };
                    (k, prev)
                })
                .collect();
            Self { vars, _lock }
        }
    }
    // `_lock` is declared last in the struct, so it drops after `Drop::drop`
    // returns: the restore runs first, then the lock releases last (matching
    // the single-var EnvGuard's drop-order invariant).
    impl Drop for MultiEnvGuard {
        fn drop(&mut self) {
            // SAFETY: ENV_LOCK is still held (self._lock), so the restore is serialized.
            unsafe {
                for (key, prev) in &self.vars {
                    match prev {
                        Some(v) => std::env::set_var(key, v),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    /// When no snapshot.json exists AND self-compose has nothing to compose
    /// (no OpenAI key, no Codex files), `statusline_cross_provider` returns
    /// `None` (Claude-only). Deterministic: sets all relevant env vars to
    /// empty/temp dirs so the test makes no network calls and is unaffected by
    /// the developer's real Codex or OpenAI configuration.
    #[test]
    fn cross_provider_none_when_snapshot_absent_and_no_self_compose_data() {
        let data_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let codex_dir = tempfile::tempdir().unwrap();
        let _guard = MultiEnvGuard::set(&[
            ("BALANZE_DATA_DIR_OVERRIDE", data_dir.path().as_os_str()),
            ("BALANZE_CACHE_DIR_OVERRIDE", cache_dir.path().as_os_str()),
            // Empty key -> resolve_openai_key() returns Ok(None) -> no network.
            ("BALANZE_OPENAI_KEY", std::ffi::OsStr::new("")),
            // Empty dir -> codex_local finds no sessions -> codex cell is None.
            ("CODEX_CONFIG_DIR", codex_dir.path().as_os_str()),
        ]);
        // No snapshot.json -> self-compose triggers.
        // No OpenAI key + no Codex data -> self-compose yields no cells.
        // pick_cross(Some(empty), None) -> None.
        assert!(super::statusline_cross_provider().is_none());
    }

    fn cp(codex: Option<f32>, openai: Option<i64>) -> statusline_render::CrossProvider {
        statusline_render::CrossProvider {
            codex_used_percent: codex,
            openai_cost_micro_usd: openai,
            codex_stale: false,
            openai_stale: false,
        }
    }

    #[test]
    fn pick_cross_prefers_composed_with_cells() {
        let composed = cp(Some(5.0), None);
        let snap = cp(None, Some(99));
        let got = super::pick_cross(Some(composed), Some(snap)).unwrap();
        assert_eq!(got.codex_used_percent, Some(5.0));
        assert_eq!(got.openai_cost_micro_usd, None);
    }

    #[test]
    fn pick_cross_falls_back_to_stale_snapshot_when_composed_empty() {
        let composed = cp(None, None);
        let snap = cp(None, Some(99));
        let got = super::pick_cross(Some(composed), Some(snap)).unwrap();
        assert_eq!(got.openai_cost_micro_usd, Some(99));
    }

    #[test]
    fn pick_cross_falls_back_when_composed_absent() {
        let snap = cp(Some(1.0), None);
        let got = super::pick_cross(None, Some(snap)).unwrap();
        assert_eq!(got.codex_used_percent, Some(1.0));
    }

    #[test]
    fn pick_cross_none_when_nothing_available() {
        assert!(super::pick_cross(Some(cp(None, None)), None).is_none());
        assert!(super::pick_cross(None, None).is_none());
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
