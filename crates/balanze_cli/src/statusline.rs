//! `statusline` subcommand: Claude Code's statusLine command. Reads the
//! statusLine JSON on stdin, prints the configured multi-line status, and
//! atomically writes the snapshot file the watcher reads.

use anyhow::Result;
use std::io::Write;

/// `balanze-cli statusline restore`: put back the foreign statusLine Balanze
/// displaced via a replace (or unwire if none was stored), then clear the
/// backup. Does NOT read stdin - distinct from the frozen render contract.
pub(crate) fn cmd_statusline_restore() -> Result<()> {
    let path = match claude_statusline::locate_settings_path() {
        Ok(p) => p,
        Err(_) => claude_statusline::default_settings_path(),
    };
    // A malformed settings.json here holds the very backup we are trying to
    // restore; defaulting would discard it. Bail so the file stays intact and
    // the user can fix it (or hand-copy the command back).
    let mut settings = settings::load_for_update()
        .map_err(|e| anyhow::anyhow!("{}: {e}", settings::UPDATE_LOAD_HINT))?;
    let previous = settings.statusline.replaced_command.take();
    let wrote = claude_statusline::restore_statusline(&path, previous.as_deref())
        .map_err(|e| anyhow::anyhow!("failed to restore statusLine at {}: {e}", path.display()))?;
    if wrote {
        // The backup was consumed - persist the now-cleared value.
        settings::save(&settings).map_err(|e| {
            anyhow::anyhow!("statusLine restored, but clearing the backup failed: {e}")
        })?;
        match previous {
            Some(cmd) => println!("Restored the previous statusLine command: {cmd}"),
            None => println!("Unwired Balanze's statusLine."),
        }
    } else if previous.is_some() {
        // A foreign command occupies the stanza; leave it and KEEP the backup
        // (do not save the cleared value) so it can be restored later.
        println!(
            "Claude Code's statusLine is set to another command; not overwriting it. \
             Your backup is kept - restore once Balanze owns the statusLine again."
        );
    } else {
        println!("No replaced command was stored; nothing to restore.");
    }
    Ok(())
}

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

/// True when any configured line asks for the `{openai_cost}` segment. When
/// false, the self-compose path skips the OpenAI cost entirely: no cache read,
/// no refresh lease, no HTTP. The segment is off in the default template, so
/// this is the common case.
///
/// The predicate defers to `statusline_render::template_uses_segment`, the same
/// authority the renderer uses to decide whether a line draws a segment. Sharing
/// it means the gate can never be broader than what the renderer will actually
/// draw - e.g. `"spend:{openai_cost}"` is literal text to the renderer, so it
/// must not set the gate and make Balanze poll OpenAI for a value that could
/// never be displayed.
fn want_openai(config: &settings::StatuslineConfig) -> bool {
    config
        .lines
        .iter()
        .any(|l| statusline_render::template_uses_segment(l, "openai_cost"))
}

/// Resolve cross-provider data (Codex %, OpenAI $) for the statusline.
///
/// Precedence: a fresh host-written `snapshot.json` wins (zero network);
/// otherwise self-compose Codex + OpenAI directly (AGENTS.md §3.1: never via the
/// OAuth-touching composer), then merge with a stale snapshot per cell so the
/// line never blanks; otherwise Claude-only.
fn statusline_cross_provider(
    config: &settings::StatuslineConfig,
) -> Option<statusline_render::CrossProvider> {
    let now = chrono::Utc::now();
    let want_openai = want_openai(config);

    // Read the host snapshot once: it feeds both the fresh-path short-circuit and
    // the seed that lets the self-compose OpenAI gate honor the watcher's fetch.
    let payload = read_snapshot_payload();
    let snapshot_cross = payload.as_ref().map(|p| cross_from_payload(p, now));

    // 1. Fresh snapshot wins (zero network). A stale OpenAI cell is only a
    //    reason to self-compose when the OpenAI segment is actually rendered.
    if let Some(cross) = &snapshot_cross {
        if (!want_openai || !cross.openai_stale) && !cross.codex_stale {
            return snapshot_cross;
        }
    }

    // 2. Self-compose; then merge composed cells over the (stale) snapshot
    //    cells per cell so a last-known value stays visible (never-blank).
    pick_cross(self_compose_cross(now, want_openai), snapshot_cross)
}

/// Merge the self-composed result with a (possibly stale) snapshot, once the
/// fresh-snapshot short-circuit has been ruled out. Each cell is taken from
/// self-compose when present (current) and otherwise from the stale snapshot
/// (shown with its `⚠` marker), so a last-known value stays visible when only
/// one source has it. `None` when neither has any cell. Pure - unit-tested.
///
/// When `want_openai` was false, `composed.openai_cost_micro_usd` is always
/// `None`, so a stale snapshot's OpenAI cell can still flow through here into
/// the merged result. That is harmless: `render_segment("openai_cost", ...)`
/// only runs for a template containing `{openai_cost}`, which is exactly the
/// condition that makes `want_openai` true - so a merged OpenAI cell carried
/// through while `want_openai` is false is structurally unreachable from any
/// render path.
fn pick_cross(
    composed: Option<statusline_render::CrossProvider>,
    stale_snapshot: Option<statusline_render::CrossProvider>,
) -> Option<statusline_render::CrossProvider> {
    let merged = match (composed, stale_snapshot) {
        (Some(c), Some(s)) => statusline_render::CrossProvider {
            codex_five_hour: c.codex_five_hour.or(s.codex_five_hour),
            codex_weekly: c.codex_weekly.or(s.codex_weekly),
            openai_cost_micro_usd: c.openai_cost_micro_usd.or(s.openai_cost_micro_usd),
            codex_stale: if c.codex_five_hour.is_some() || c.codex_weekly.is_some() {
                c.codex_stale
            } else {
                s.codex_stale
            },
            openai_stale: if c.openai_cost_micro_usd.is_some() {
                c.openai_stale
            } else {
                s.openai_stale
            },
        },
        (Some(c), None) => c,
        (None, Some(s)) => s,
        (None, None) => return None,
    };
    (merged.codex_five_hour.is_some()
        || merged.codex_weekly.is_some()
        || merged.openai_cost_micro_usd.is_some())
    .then_some(merged)
}

/// Run the self-compose path: build the OAuth-free `LiveCrossSources`, a
/// one-shot runtime, and call `statusline_render::self_compose`. `None` if the
/// runtime cannot be built (or, when the OpenAI segment is wanted, there is no
/// cache dir); never panics when called from a synchronous context (the
/// `block_on` below would panic if called from within an async runtime).
///
/// When `want_openai` is false the OpenAI leg is fully inert: `resolve` skips
/// the keychain read, and the cache dir and fingerprint are never resolved -
/// Codex composition needs none of them. That keeps the shipped default
/// statusline from touching the OpenAI keychain (a possible macOS prompt or
/// latency) every prompt turn for a value that could not render.
fn self_compose_cross(
    now: chrono::DateTime<chrono::Utc>,
    want_openai: bool,
) -> Option<statusline_render::CrossProvider> {
    let sources = crate::sources::LiveCrossSources::resolve(want_openai);
    // The cache dir and fingerprint are consumed only on the OpenAI leg. With
    // the segment off, do not require the cache dir (Codex has no such
    // dependency) and do not compute the fingerprint (no key was resolved to
    // feed it) - self_compose ignores both when want_openai is false.
    let (cache_dir, fingerprint) = if want_openai {
        (
            statusline_render::cache::cache_dir_path()?,
            sources.openai_fingerprint(),
        )
    } else {
        (std::path::PathBuf::new(), String::new())
    };
    // One-shot CLI: a fresh per-turn runtime is acceptable; the OpenAI fetch
    // inside self_compose is cache-gated (300s) and skipped entirely when the
    // segment is not configured, so the network is not hit every turn even
    // though the runtime is built every turn.
    let rt = tokio::runtime::Runtime::new().ok()?;
    Some(rt.block_on(statusline_render::self_compose(
        &sources,
        &cache_dir,
        &fingerprint,
        now,
        want_openai,
    )))
}

/// Read the host-written `snapshot.json` payload. `None` only when the file is
/// absent or unreadable.
fn read_snapshot_payload() -> Option<state_coordinator::SnapshotFilePayload> {
    let path = state_coordinator::snapshot_file_path()?;
    match state_coordinator::read_snapshot_file(&path) {
        Ok(payload) => Some(payload),
        Err(state_coordinator::SnapshotFileError::FileMissing { .. }) => None,
        Err(e) => {
            tracing::debug!(
                "statusline: cross-provider snapshot unreadable, trying self-compose: {e}"
            );
            None
        }
    }
}

/// Pure map: snapshot-file payload -> `CrossProvider`. A cell is stale when the
/// whole snapshot is old (envelope age) OR its own source reported an error on
/// its last poll (stale-but-known data is still shown with a marker rather than
/// hidden - the project's stale-with-indicator rule).
fn cross_from_payload(
    payload: &state_coordinator::SnapshotFilePayload,
    now: chrono::DateTime<chrono::Utc>,
) -> statusline_render::CrossProvider {
    let snap = &payload.snapshot;
    let age = now.signed_duration_since(payload.captured_at).num_seconds();
    // The host rewrites the envelope on every coordinator update, so its age
    // only tells us the whole snapshot is old. A single source can be stale
    // while the envelope is young (e.g. Claude JSONL keeps updating while OpenAI
    // polls fail), so also mark a cell stale when its source last errored.
    let envelope_stale = age > SNAPSHOT_FRESHNESS_SECS;
    // A FRESH envelope can still carry an EXPIRED Codex window: the rollout
    // walker returns the newest-mtime session file however old it is, so a
    // user who last ran Codex days ago gets a young envelope wrapping windows
    // that reset long ago. Neither the envelope age nor the error slot can see
    // that, so without this the cell prints a confident live figure forever.
    // Anchored on wall-clock `now` (the statusline has a real clock, unlike the
    // snapshot-rendering surfaces that anchor on `fetched_at`).
    let codex_expired = snap
        .codex_quota
        .as_ref()
        .is_some_and(|q| q.any_window_expired(now));
    statusline_render::CrossProvider {
        codex_five_hour: snap
            .codex_quota
            .as_ref()
            .and_then(|q| q.five_hour())
            .map(|w| w.used_percent as f32),
        codex_weekly: snap
            .codex_quota
            .as_ref()
            .and_then(|q| q.weekly())
            .map(|w| w.used_percent as f32),
        openai_cost_micro_usd: snap.openai.as_ref().map(|c| c.total_micro_usd),
        codex_stale: envelope_stale || snap.codex_quota_error.is_some() || codex_expired,
        openai_stale: envelope_stale || snap.openai_error.is_some(),
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
    let cross = statusline_cross_provider(&settings.statusline);
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
    let Some(path) = settings::statusline_snapshot_path() else {
        tracing::warn!("statusline: could not resolve data dir; skipping snapshot write");
        return;
    };
    let envelope = claude_statusline::StatuslineFilePayload::new(snap.clone(), chrono::Utc::now());
    if let Err(e) = claude_statusline::atomic_write_snapshot(&path, &envelope) {
        tracing::warn!("statusline: snapshot write failed: {e}");
    }
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
            tokens: None,
            credits: None,
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
        // The single window is weekly (10_080 min), so it lands in codex_weekly.
        assert_eq!(c.codex_weekly, Some(6.0));
        assert_eq!(c.codex_five_hour, None);
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
    fn cross_surfaces_both_codex_windows() {
        use chrono::TimeZone as _;
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 8, 12, 0, 0).unwrap();
        let mut s = state_coordinator::Snapshot::empty(now);
        s.codex_quota = Some(codex_local::types::CodexQuotaSnapshot {
            observed_at: now,
            session_id: "s".into(),
            primary: codex_local::types::RateLimitWindow {
                used_percent: 1.0,
                window_duration_minutes: 300,
                resets_at: now,
            },
            secondary: Some(codex_local::types::RateLimitWindow {
                used_percent: 6.0,
                window_duration_minutes: 10_080,
                resets_at: now,
            }),
            plan_type: "pro".into(),
            rate_limit_reached: false,
            tokens: None,
            credits: None,
        });
        let payload = state_coordinator::SnapshotFilePayload::new(s, now);
        let c = super::cross_from_payload(&payload, now);
        // Both windows surface independently: 5h at 1% and weekly at 6%.
        assert_eq!(c.codex_five_hour, Some(1.0));
        assert_eq!(c.codex_weekly, Some(6.0));
    }

    #[test]
    fn cross_from_payload_marks_errored_cells_stale_despite_fresh_envelope() {
        use chrono::TimeZone as _;
        let now = chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap();
        let mut s = state_coordinator::Snapshot::empty(now);
        // Coordinator keeps the last-known values...
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
            tokens: None,
            credits: None,
        });
        s.openai = Some(openai_client::OpenAiCosts {
            start_time: now,
            end_time: now,
            total_micro_usd: 4_200_000,
            by_line_item: vec![],
            truncated: false,
            fetched_at: now,
        });
        // ...but each source's last poll failed.
        s.codex_quota_error = Some("codex boom".into());
        s.openai_error = Some("openai boom".into());
        // Envelope is fresh (now), yet the errored cells must render stale.
        let payload = state_coordinator::SnapshotFilePayload::new(s, now);
        let c = super::cross_from_payload(&payload, now);
        // The single window is weekly (10_080 min).
        assert_eq!(c.codex_weekly, Some(6.0));
        assert_eq!(c.openai_cost_micro_usd, Some(4_200_000));
        assert!(
            c.codex_stale,
            "errored codex cell stale despite fresh envelope"
        );
        assert!(
            c.openai_stale,
            "errored openai cell stale despite fresh envelope"
        );
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
        assert!(c.codex_five_hour.is_none());
        assert!(c.codex_weekly.is_none());
        assert!(c.openai_cost_micro_usd.is_none());
    }

    #[test]
    fn cross_renders_codex_and_openai_segments() {
        let snap = claude_statusline::parse(r#"{"model":{"display_name":"Opus"}}"#).unwrap();
        let cross = statusline_render::CrossProvider {
            codex_five_hour: Some(6.0),
            codex_weekly: None,
            openai_cost_micro_usd: Some(4_200_000),
            codex_stale: false,
            openai_stale: false,
        };
        // `openai_cost` is off by default (see `default_lines`), so this test -
        // which exercises the glue rendering both segments together - asks for
        // it explicitly rather than relying on the shipped default template.
        let config = settings::StatuslineConfig {
            lines: vec!["{codex} {openai_cost}".to_string()],
            ..Default::default()
        };
        let out = super::render_with(&snap, &config, false, Some(&cross));
        assert!(out.contains("🌀 5h 6%"), "{out}");
        assert!(out.contains("🌀 $4.20"), "{out}");
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
        assert!(super::statusline_cross_provider(&settings::StatuslineConfig::default()).is_none());
    }

    #[test]
    fn want_openai_follows_the_configured_lines() {
        let asks = settings::StatuslineConfig {
            lines: vec!["{usage} {openai_cost}".to_string()],
            ..Default::default()
        };
        assert!(super::want_openai(&asks), "template asks for the segment");

        let silent = settings::StatuslineConfig {
            lines: vec!["{usage} {codex}".to_string()],
            ..Default::default()
        };
        assert!(
            !super::want_openai(&silent),
            "template does not ask for the segment"
        );
    }

    #[test]
    fn default_template_does_not_want_openai() {
        assert!(
            !super::want_openai(&settings::StatuslineConfig::default()),
            "the shipped default template must not request the OpenAI segment"
        );
    }

    /// The gate must match `fill_line`'s token rule exactly: a `{openai_cost}`
    /// token only counts as a placeholder when it is a whole whitespace-delimited
    /// token, not a substring of one. `render.rs::fill_line` splits on whitespace
    /// and requires an exact `{key}` match, so `"spend:{openai_cost}"` is literal
    /// text there - the OpenAI segment never renders for that template. Before
    /// this fix, `want_openai`'s substring `contains` check set the gate true
    /// anyway, polling OpenAI for a value that could never be displayed.
    #[test]
    fn want_openai_matches_the_renderer_exact_token_rule() {
        let glued = settings::StatuslineConfig {
            lines: vec!["spend:{openai_cost}".to_string()],
            ..Default::default()
        };
        assert!(
            !super::want_openai(&glued),
            "a glued substring must not set the gate: the renderer treats it as literal text"
        );

        let trailing_punct = settings::StatuslineConfig {
            lines: vec!["{openai_cost}.".to_string()],
            ..Default::default()
        };
        assert!(
            !super::want_openai(&trailing_punct),
            "trailing punctuation on the token must not set the gate either"
        );

        let exact = settings::StatuslineConfig {
            lines: vec!["{usage} {openai_cost}".to_string()],
            ..Default::default()
        };
        assert!(
            super::want_openai(&exact),
            "an exact whitespace-delimited token must set the gate"
        );
    }

    fn cp(codex: Option<f32>, openai: Option<i64>) -> statusline_render::CrossProvider {
        statusline_render::CrossProvider {
            codex_five_hour: codex,
            codex_weekly: None,
            openai_cost_micro_usd: openai,
            codex_stale: false,
            openai_stale: false,
        }
    }

    fn cp_stale(codex: Option<f32>, openai: Option<i64>) -> statusline_render::CrossProvider {
        statusline_render::CrossProvider {
            codex_five_hour: codex,
            codex_weekly: None,
            openai_cost_micro_usd: openai,
            codex_stale: true,
            openai_stale: true,
        }
    }

    #[test]
    fn pick_cross_merges_composed_over_stale_snapshot_cells() {
        // Composed has fresh Codex; the stale snapshot has an OpenAI value. The
        // merge keeps the fresh Codex AND surfaces the stale OpenAI (with its
        // marker) rather than dropping it.
        let got =
            super::pick_cross(Some(cp(Some(5.0), None)), Some(cp_stale(None, Some(99)))).unwrap();
        assert_eq!(got.codex_five_hour, Some(5.0));
        assert!(!got.codex_stale, "composed Codex is current");
        assert_eq!(got.openai_cost_micro_usd, Some(99));
        assert!(got.openai_stale, "snapshot OpenAI is stale");
    }

    #[test]
    fn pick_cross_prefers_composed_cell_when_both_have_it() {
        // Both sources have OpenAI; the composed (current) value wins.
        let got =
            super::pick_cross(Some(cp(None, Some(42))), Some(cp_stale(None, Some(99)))).unwrap();
        assert_eq!(got.openai_cost_micro_usd, Some(42));
        assert!(!got.openai_stale);
    }

    #[test]
    fn pick_cross_falls_back_to_stale_snapshot_when_composed_empty() {
        let got = super::pick_cross(Some(cp(None, None)), Some(cp_stale(None, Some(99)))).unwrap();
        assert_eq!(got.openai_cost_micro_usd, Some(99));
        assert!(got.openai_stale);
    }

    #[test]
    fn pick_cross_falls_back_when_composed_absent() {
        let got = super::pick_cross(None, Some(cp_stale(Some(1.0), None))).unwrap();
        assert_eq!(got.codex_five_hour, Some(1.0));
        assert!(got.codex_stale);
    }

    #[test]
    fn pick_cross_none_when_nothing_available() {
        assert!(super::pick_cross(Some(cp(None, None)), None).is_none());
        assert!(super::pick_cross(None, None).is_none());
        assert!(super::pick_cross(Some(cp(None, None)), Some(cp_stale(None, None))).is_none());
    }

    #[test]
    fn statusline_snapshot_path_honors_env_override() {
        let _guard = EnvGuard::set("BALANZE_DATA_DIR_OVERRIDE", "/tmp/balanze-test");
        let p = settings::statusline_snapshot_path().unwrap();
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
