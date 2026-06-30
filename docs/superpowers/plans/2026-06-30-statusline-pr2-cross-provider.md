# Statusline PR2: Cross-Provider via the Watcher Snapshot - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The host that owns the coordinator (the Tauri app, and `balanze-cli watch`) atomically writes the live `Snapshot` to a new IPC file `snapshot.json` on every update; `balanze-cli statusline` reads it and fills the Codex-quota + OpenAI-billed-$ segments from it (Claude segments still come from stdin), with a staleness marker when the file is old and a graceful Claude-only fallback when it is absent or unreadable.

**Architecture:** `state_coordinator` owns `Snapshot`, so it also owns the new `snapshot.json` envelope + atomic read/write (mirroring `claude_statusline::file_io`) and a one-line `snapshot_file_path()` resolver. A `SnapshotFileSink<S>` decorator writes the file on each `on_snapshot` then delegates to the wrapped sink, keeping I/O out of the coordinator actor (boundary #7). The one-shot `balanze-cli statusline` reads the file and maps `codex_quota.primary.used_percent` + `openai.total_micro_usd` into `statusline_render::CrossProvider`. **Zero statusline-initiated network** (self-compose + cache is PR3).

**Tech Stack:** Rust 2024, `serde`/`serde_json`, `chrono`, `directories`, `thiserror`, `tracing`, `cargo nextest`. No new runtime crates beyond adding `directories` to `state_coordinator`.

**Scope note:** PR2 of the 5-PR v0.4.2 statusline release (`docs/superpowers/specs/2026-06-30-statusline-design.md`, §5.1/§5.4/§7/§9). PR3 (self-compose + per-turn cache), PR4 (replace-any-statusline flow), PR5 (Codex preset + docs/release) follow. The desktop-host writer shares the exact `SnapshotFileSink` with the CLI-`watch` writer; because `tauri dev` is currently blocked (issue #136), **the end-to-end verification path for PR2 is `balanze-cli watch` (writes the file) + `balanze-cli statusline` (reads it)** - no desktop app required.

**Key facts from exploration (so tasks are precise):**
- `Snapshot` is in `crates/state_coordinator/src/snapshot.rs:96`, `#[derive(Debug, Clone, Serialize, Deserialize)]` (NO `PartialEq`), `pub const SNAPSHOT_SCHEMA_VERSION: u32 = 2;`. Cells: `codex_quota: Option<CodexQuotaSnapshot>` (the % is `.primary.used_percent: f64`), `openai: Option<OpenAiCosts>` (`.total_micro_usd: i64`). `Snapshot::empty(now)` constructor exists. Re-exported from `state_coordinator/src/lib.rs`.
- `Sink` trait (`crates/state_coordinator/src/sink.rs:16`): `fn on_snapshot(&mut self, snapshot: &Snapshot)`, `fn on_degraded(&mut self, source: Source, error: &str)`. `spawn<S: Sink>(sink) -> (StateCoordinatorHandle, JoinHandle<()>)` takes ONE sink (no fan-out).
- `CrossProvider` (`crates/statusline_render/src/render.rs:6`): `{ codex_used_percent: Option<f32>, openai_cost_micro_usd: Option<i64>, stale: bool }`; the renderer already renders the `codex`/`openai_cost` segments from it (PR1).
- The CLI statusline path resolver pattern (`BALANZE_DATA_DIR_OVERRIDE` else `ProjectDirs::from("me","oszkar","Balanze").data_dir()`) lives in `crates/balanze_cli/src/statusline.rs:89`.

---

### Task 1: `snapshot.json` envelope, atomic IO, and path resolver

**Goal:** Add `state_coordinator::snapshot_file` - a versioned envelope around `Snapshot`, atomic read/write mirroring `claude_statusline::file_io`, and a `snapshot_file_path()` resolver.

**Files:**
- Create: `crates/state_coordinator/src/snapshot_file.rs`
- Modify: `crates/state_coordinator/src/lib.rs` (declare module + re-export)
- Modify: `crates/state_coordinator/Cargo.toml` (add `directories`; add `tempfile` dev-dep if absent)

**Acceptance Criteria:**
- [ ] `SnapshotFilePayload { schema_version: u32, captured_at: DateTime<Utc>, snapshot: Snapshot }` serializes/deserializes; `::new(snapshot, captured_at)` stamps `SNAPSHOT_SCHEMA_VERSION`.
- [ ] `atomic_write_snapshot_file` then `read_snapshot_file` round-trips (assert on `schema_version` + a cell, since `Snapshot` has no `PartialEq`).
- [ ] Missing file -> `FileMissing`; wrong `schema_version` -> `SchemaDrift`; malformed JSON -> `ParseError`; no `.tmp` left after success.
- [ ] `snapshot_file_path()` honors `BALANZE_DATA_DIR_OVERRIDE`, else `ProjectDirs ... data_dir()/snapshot.json`.

**Verify:** `cargo nextest run -p state_coordinator`; `cargo clippy -p state_coordinator --all-targets -- -D warnings`.

**Steps:**

- [ ] **Step 1: Cargo.toml** - add under `[dependencies]`: `directories = { workspace = true }`. Ensure `[dev-dependencies]` has `tempfile = "3"` (add if missing). `serde_json`, `chrono`, `thiserror`, `tracing`, `serde` are already deps.

- [ ] **Step 2: Create `crates/state_coordinator/src/snapshot_file.rs`**

```rust
//! IPC file for the cross-provider `Snapshot`: `<data_dir>/snapshot.json`.
//!
//! The OPPOSITE direction to `claude_statusline`'s `statusline.snapshot.json`:
//! the host that owns the coordinator (the Tauri app, or `balanze-cli watch`)
//! WRITES the live `Snapshot` on every coordinator update via `SnapshotFileSink`
//! (see `sink_file.rs`), and the one-shot `balanze-cli statusline` process READS
//! it to fill the cross-provider (Codex / OpenAI) segments without any network
//! I/O of its own (AGENTS.md §3.1; the statusline design's Hybrid read path).
//!
//! Atomic tmp+fsync+rename write, probe-then-parse read, path-only errors -
//! mirrors `claude_statusline::file_io`. The coordinator actor never calls these
//! (boundary #7: no I/O in the actor); the host's sink does.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::snapshot::{SNAPSHOT_SCHEMA_VERSION, Snapshot};

/// Versioned envelope written to `snapshot.json`. `captured_at` is the
/// consumer-side freshness signal (the statusline reader compares its age to a
/// TTL). `snapshot` is the full coordinator snapshot, serde round-trippable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFilePayload {
    pub schema_version: u32,
    pub captured_at: DateTime<Utc>,
    pub snapshot: Snapshot,
}

impl SnapshotFilePayload {
    pub fn new(snapshot: Snapshot, captured_at: DateTime<Utc>) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            captured_at,
            snapshot,
        }
    }
}

/// Errors from [`read_snapshot_file`] / [`atomic_write_snapshot_file`]. Every
/// variant carries the path; none carry file contents.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotFileError {
    #[error("snapshot file missing: {path}")]
    FileMissing { path: PathBuf },
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot parse error in {path}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("snapshot schema drift in {path}: found version {found_version}, expected {expected}")]
    SchemaDrift {
        path: PathBuf,
        found_version: u32,
        expected: u32,
    },
}

/// Resolve `<data_dir>/snapshot.json`. Honors `BALANZE_DATA_DIR_OVERRIDE`
/// (tests / headless) exactly like the statusline snapshot path. `None` when no
/// project dir resolves.
pub fn snapshot_file_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(PathBuf::from(dir).join("snapshot.json"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.data_dir().join("snapshot.json"))
}

/// Read + validate a [`SnapshotFilePayload`]. Probe `schema_version` first so a
/// future-versioned file yields a precise `SchemaDrift` rather than a generic
/// parse error.
pub fn read_snapshot_file(path: &Path) -> Result<SnapshotFilePayload, SnapshotFileError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SnapshotFileError::FileMissing {
                path: path.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(SnapshotFileError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    #[derive(Deserialize)]
    struct VersionProbe {
        schema_version: u32,
    }
    let probe: VersionProbe =
        serde_json::from_slice(&bytes).map_err(|e| SnapshotFileError::ParseError {
            path: path.to_path_buf(),
            source: e,
        })?;
    if probe.schema_version != SNAPSHOT_SCHEMA_VERSION {
        return Err(SnapshotFileError::SchemaDrift {
            path: path.to_path_buf(),
            found_version: probe.schema_version,
            expected: SNAPSHOT_SCHEMA_VERSION,
        });
    }

    serde_json::from_slice(&bytes).map_err(|e| SnapshotFileError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Atomically write `payload` via tmp+fsync+rename. Creates parent dirs; leaves
/// no tmp on success; preserves existing perms on unix.
pub fn atomic_write_snapshot_file(
    path: &Path,
    payload: &SnapshotFilePayload,
) -> Result<(), SnapshotFileError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).map_err(|e| SnapshotFileError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;

    let bytes = serde_json::to_vec_pretty(payload).map_err(|e| SnapshotFileError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })?;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let tmp = parent.join(format!(
        "snapshot.{}-{}-{}.json.tmp",
        std::process::id(),
        nanos,
        seq,
    ));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create_new(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(SnapshotFileError::Io { path: tmp, source: e });
    }

    #[cfg(unix)]
    {
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        SnapshotFileError::Io {
            path: path.to_path_buf(),
            source: e,
        }
    })?;

    #[cfg(unix)]
    {
        let _ = std::fs::File::open(parent).and_then(|f| f.sync_all());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Snapshot;
    use chrono::TimeZone as _;
    use tempfile::tempdir;

    fn payload_with_codex() -> SnapshotFilePayload {
        let now = chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap();
        let mut snap = Snapshot::empty(now);
        snap.codex_quota = Some(codex_local::types::CodexQuotaSnapshot {
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
        SnapshotFilePayload::new(snap, now)
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        atomic_write_snapshot_file(&path, &payload_with_codex()).unwrap();
        let back = read_snapshot_file(&path).unwrap();
        assert_eq!(back.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(
            back.snapshot.codex_quota.unwrap().primary.used_percent,
            6.0
        );
    }

    #[test]
    fn missing_file_is_file_missing() {
        let dir = tempdir().unwrap();
        let err = read_snapshot_file(&dir.path().join("nope.json")).unwrap_err();
        assert!(matches!(err, SnapshotFileError::FileMissing { .. }), "{err:?}");
    }

    #[test]
    fn wrong_schema_version_is_drift() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(
            &path,
            br#"{"schema_version":999,"captured_at":"2026-06-30T12:00:00Z","snapshot":{}}"#,
        )
        .unwrap();
        match read_snapshot_file(&path).unwrap_err() {
            SnapshotFileError::SchemaDrift { found_version, .. } => assert_eq!(found_version, 999),
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_parse_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            read_snapshot_file(&path).unwrap_err(),
            SnapshotFileError::ParseError { .. }
        ));
    }

    #[test]
    fn no_tmp_left_after_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        atomic_write_snapshot_file(&path, &payload_with_codex()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn path_honors_env_override() {
        // Safe to set in this single-threaded test; mirrors the statusline test
        // pattern. If the crate's test harness runs env-mutating tests in
        // parallel, gate with the same serialization the statusline tests use.
        unsafe { std::env::set_var("BALANZE_DATA_DIR_OVERRIDE", "/tmp/balanze-x") };
        let p = snapshot_file_path().unwrap();
        unsafe { std::env::remove_var("BALANZE_DATA_DIR_OVERRIDE") };
        assert!(p.ends_with("snapshot.json"));
        assert!(p.to_string_lossy().contains("balanze-x"));
    }
}
```

NOTE: the test references `codex_local::types::*`. If `codex_local` is not already a dev-dependency of `state_coordinator`, add `codex_local = { path = "../codex_local" }` under `[dev-dependencies]`. (It is almost certainly already a normal dep, since `Snapshot.codex_quota` is `Option<codex_local::types::CodexQuotaSnapshot>` - confirm and skip if so.)

- [ ] **Step 3: Declare + re-export in `crates/state_coordinator/src/lib.rs`**

Add near the other `pub mod` / `pub use` lines:
```rust
pub mod snapshot_file;
pub use snapshot_file::{
    SnapshotFileError, SnapshotFilePayload, atomic_write_snapshot_file, read_snapshot_file,
    snapshot_file_path,
};
```

- [ ] **Step 4: Verify** - `cargo nextest run -p state_coordinator` (6 new tests green), `cargo clippy -p state_coordinator --all-targets -- -D warnings`, `cargo fmt -p state_coordinator`.

- [ ] **Step 5: Commit** - `feat(snapshot): add snapshot.json envelope, atomic IO, and path resolver`

---

### Task 2: `SnapshotFileSink<S>` writer decorator

**Goal:** A `Sink` that persists the full `Snapshot` to `snapshot.json` on each update, then delegates to the wrapped sink. Best-effort: a write failure logs at `warn!` and never breaks the inner sink or the coordinator.

**Files:**
- Create: `crates/state_coordinator/src/sink_file.rs`
- Modify: `crates/state_coordinator/src/lib.rs` (declare + re-export `SnapshotFileSink`)

**Acceptance Criteria:**
- [ ] `SnapshotFileSink::new(inner, path)` wraps any `S: Sink`.
- [ ] `on_snapshot` writes `snapshot.json` at `path` AND calls `inner.on_snapshot`.
- [ ] `on_degraded` delegates unchanged.
- [ ] A write failure (e.g. an un-creatable path) does not panic and the inner sink is still called.

**Verify:** `cargo nextest run -p state_coordinator`; `cargo clippy -p state_coordinator --all-targets -- -D warnings`.

**Steps:**

- [ ] **Step 1: Write failing tests in `crates/state_coordinator/src/sink_file.rs`**

```rust
//! `SnapshotFileSink` - the WRITE side of the cross-provider Hybrid read path.
//!
//! Wraps any `Sink`; on each `on_snapshot` it persists the full `Snapshot` to
//! `snapshot.json` (best-effort, errors logged not propagated) then delegates to
//! the inner sink. The host (Tauri app / `balanze-cli watch`) wraps its real
//! sink with this; the one-shot `balanze-cli statusline` reads the file. Keeps
//! file I/O on the sink side, never in the coordinator actor (boundary #7).

use std::path::PathBuf;

use chrono::Utc;

use crate::messages::Source;
use crate::sink::Sink;
use crate::snapshot::Snapshot;
use crate::snapshot_file::{SnapshotFilePayload, atomic_write_snapshot_file};

pub struct SnapshotFileSink<S: Sink> {
    inner: S,
    path: PathBuf,
}

impl<S: Sink> SnapshotFileSink<S> {
    pub fn new(inner: S, path: PathBuf) -> Self {
        Self { inner, path }
    }
}

impl<S: Sink> Sink for SnapshotFileSink<S> {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        let payload = SnapshotFilePayload::new(snapshot.clone(), Utc::now());
        if let Err(e) = atomic_write_snapshot_file(&self.path, &payload) {
            tracing::warn!("snapshot.json write failed: {e}");
        }
        self.inner.on_snapshot(snapshot);
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        self.inner.on_degraded(source, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Snapshot;
    use crate::snapshot_file::read_snapshot_file;
    use chrono::TimeZone as _;
    use tempfile::tempdir;

    #[derive(Default)]
    struct CountingSink {
        snapshots: usize,
        degraded: usize,
    }
    impl Sink for CountingSink {
        fn on_snapshot(&mut self, _s: &Snapshot) {
            self.snapshots += 1;
        }
        fn on_degraded(&mut self, _src: Source, _e: &str) {
            self.degraded += 1;
        }
    }

    fn snap() -> Snapshot {
        Snapshot::empty(chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap())
    }

    #[test]
    fn writes_file_and_delegates_on_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let mut sink = SnapshotFileSink::new(CountingSink::default(), path.clone());
        sink.on_snapshot(&snap());
        assert_eq!(sink.inner.snapshots, 1, "inner sink still called");
        let back = read_snapshot_file(&path).expect("file written");
        assert_eq!(back.schema_version, crate::snapshot::SNAPSHOT_SCHEMA_VERSION);
    }

    #[test]
    fn on_degraded_delegates() {
        let dir = tempdir().unwrap();
        let mut sink =
            SnapshotFileSink::new(CountingSink::default(), dir.path().join("snapshot.json"));
        sink.on_degraded(Source::CodexQuota, "boom");
        assert_eq!(sink.inner.degraded, 1);
    }

    #[test]
    fn write_failure_does_not_break_inner() {
        // Point at a path whose parent is a FILE, so create_dir_all/create fails.
        let dir = tempdir().unwrap();
        let file_as_parent = dir.path().join("a_file");
        std::fs::write(&file_as_parent, b"x").unwrap();
        let bad_path = file_as_parent.join("snapshot.json");
        let mut sink = SnapshotFileSink::new(CountingSink::default(), bad_path);
        sink.on_snapshot(&snap()); // must not panic
        assert_eq!(sink.inner.snapshots, 1, "inner sink still called on write failure");
    }
}
```
Run `cargo nextest run -p state_coordinator` -> FAILS to compile (module not declared).

- [ ] **Step 2: Declare + re-export in `crates/state_coordinator/src/lib.rs`**
```rust
mod sink_file;
pub use sink_file::SnapshotFileSink;
```

- [ ] **Step 3: Verify** - tests green, clippy clean, fmt.

- [ ] **Step 4: Commit** - `feat(snapshot): SnapshotFileSink writes snapshot.json on each coordinator update`

---

### Task 3: Wire the writer into both hosts

**Goal:** Wrap the coordinator sink with `SnapshotFileSink` in the Tauri host and in the CLI `watch` paths, so a running host keeps `snapshot.json` fresh.

**Files:**
- Modify: `src-tauri/src/lib.rs` (the `boot_backend` spawn site, ~line 334)
- Modify: `crates/balanze_cli/src/watch_cmd.rs` (the `run_with_sink` spawn site ~line 60, and `run_tui_mode` ~line 150)

**Acceptance Criteria:**
- [ ] Tauri host: when `snapshot_file_path()` resolves, the coordinator runs with `SnapshotFileSink::new(TauriSink, path)`; otherwise it falls back to the bare `TauriSink` with a `warn!`.
- [ ] CLI `watch` (both the streaming and TUI paths): same wrap of their respective sinks.
- [ ] `cargo build --workspace` succeeds; existing tests stay green.

**Verify:** `cargo build --workspace`; `cargo nextest run -p balanze_cli`; manual: `BALANZE_DATA_DIR_OVERRIDE=<tmp> balanze-cli watch` for ~5s writes `<tmp>/snapshot.json` (see Task 4 manual step). `src-tauri` clippy/build (tray smoke deferred to #136).

**Steps:**

- [ ] **Step 1: `src-tauri/src/lib.rs`** - replace the spawn at ~line 334-335:
```rust
    let sink = TauriSink::new(app.handle().clone());
    let (handle, coord_join) = match state_coordinator::snapshot_file_path() {
        Some(path) => state_coordinator::spawn(state_coordinator::SnapshotFileSink::new(sink, path)),
        None => {
            tracing::warn!(
                "could not resolve snapshot.json path; cross-provider statusline will fall back to Claude-only"
            );
            state_coordinator::spawn(sink)
        }
    };
```
(Both match arms return `(StateCoordinatorHandle, JoinHandle<()>)` - `spawn`'s return type is independent of the sink type - so the `let` unifies. `sink` is moved in exactly one arm.)

- [ ] **Step 2: `crates/balanze_cli/src/watch_cmd.rs`** - at each `spawn_coord(sink)` site (the streaming path ~line 60 and the TUI path ~line 150), wrap the sink the same way. Read the exact local around each call first; the pattern is:
```rust
    let (handle, join) = match state_coordinator::snapshot_file_path() {
        Some(path) => spawn_coord(state_coordinator::SnapshotFileSink::new(sink, path)),
        None => spawn_coord(sink),
    };
```
Adapt to the actual binding names and the `spawn_coord` signature (it is generic over the sink type). If `spawn_coord` is a thin alias for `state_coordinator::spawn`, the wrap is identical to Step 1.

- [ ] **Step 3: Verify** - `cargo build --workspace`, `cargo nextest run -p balanze_cli`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`.

- [ ] **Step 4: Commit** - `feat(statusline): write snapshot.json from the Tauri host and CLI watch`

---

### Task 4: Reader + Snapshot->CrossProvider mapper in `cmd_statusline`

**Goal:** `balanze-cli statusline` reads `snapshot.json`, maps the Codex/OpenAI cells into `CrossProvider` (with a TTL-based `stale` flag), and passes it to the renderer; absent/unreadable -> Claude-only.

**Files:**
- Modify: `crates/balanze_cli/src/statusline.rs` (reader + mapper + thread `cross` into `render_with`; tests)
- Modify: `crates/balanze_cli/Cargo.toml` (add `codex_local` + `openai_client` dev-deps if needed for the mapper test)

**Acceptance Criteria:**
- [ ] `cross_from_payload` maps `codex_quota.primary.used_percent` (f64->f32) and `openai.total_micro_usd` (i64); `stale = (now - captured_at) > 120s`.
- [ ] A fresh payload -> `Some(cross)` with `stale=false`; an old payload -> `stale=true`; an empty snapshot -> both cells `None`.
- [ ] `statusline_cross_provider()` returns `None` when the file is absent / parse-errors / schema-drifts (Claude-only fallback).
- [ ] `render_line` passes the cross-provider through; the existing snapshot-write + render tests stay green.

**Verify:** `cargo nextest run -p balanze_cli`; `cargo clippy --workspace --all-targets -- -D warnings`; manual end-to-end (Step 5).

**Steps:**

- [ ] **Step 1: Add the reader + mapper to `crates/balanze_cli/src/statusline.rs`**

```rust
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
        Err(_) => None,
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
```

- [ ] **Step 2: Thread `cross` through `render_line` / `render_with`** (replace the PR1 bodies):
```rust
fn render_line(snap: &claude_statusline::StatuslineSnapshot) -> String {
    let settings = settings::load().unwrap_or_default();
    let color = std::env::var_os("NO_COLOR").is_none();
    let cross = statusline_cross_provider();
    render_with(snap, &settings.statusline, color, cross.as_ref())
}

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
```

- [ ] **Step 3: Update the existing `render_with_default_config_contains_known_segments` test** to pass the new arg, and add mapper + cross-render tests. (`Cargo.toml`: add `codex_local`/`openai_client` dev-deps if the mapper test needs to build the cells - confirm whether they are already deps first.)
```rust
    #[test]
    fn render_with_default_config_contains_known_segments() {
        let snap = claude_statusline::parse(
            r#"{"rate_limits":{"five_hour":{"used_percentage":82,"resets_at":4102444800}},"cost":{"total_cost_usd":2.5},"model":{"display_name":"Opus"}}"#,
        )
        .unwrap();
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
        s.openai = Some(openai_client::types::OpenAiCosts {
            start_time: now,
            end_time: now,
            total_micro_usd: 4_200_000,
            by_line_item: vec![],
            truncated: false,
            fetched_at: now,
        });
        // Fresh: captured now.
        let fresh = state_coordinator::SnapshotFilePayload::new(s.clone(), now);
        let c = super::cross_from_payload(&fresh, now);
        assert_eq!(c.codex_used_percent, Some(6.0));
        assert_eq!(c.openai_cost_micro_usd, Some(4_200_000));
        assert!(!c.stale, "fresh payload is not stale");
        // Stale: captured 200s ago.
        let stale_payload =
            state_coordinator::SnapshotFilePayload::new(s, now - chrono::Duration::seconds(200));
        assert!(super::cross_from_payload(&stale_payload, now).stale);
    }

    #[test]
    fn cross_from_empty_snapshot_has_no_cells() {
        use chrono::TimeZone as _;
        let now = chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap();
        let payload =
            state_coordinator::SnapshotFilePayload::new(state_coordinator::Snapshot::empty(now), now);
        let c = super::cross_from_payload(&payload, now);
        assert!(c.codex_used_percent.is_none());
        assert!(c.openai_cost_micro_usd.is_none());
    }

    #[test]
    fn cross_renders_codex_and_openai_segments() {
        // A populated cross-provider renders the codex + openai_cost segments.
        let snap = claude_statusline::parse(r#"{"model":{"display_name":"Opus"}}"#).unwrap();
        let cross = statusline_render::CrossProvider {
            codex_used_percent: Some(6.0),
            openai_cost_micro_usd: Some(4_200_000),
            stale: false,
        };
        let out =
            super::render_with(&snap, &settings::StatuslineConfig::default(), false, Some(&cross));
        assert!(out.contains("Codex 6%"), "{out}");
        assert!(out.contains("OpenAI $4.20"), "{out}");
    }
```

- [ ] **Step 4: Verify** - `cargo nextest run -p balanze_cli`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`.

- [ ] **Step 5: Manual end-to-end (the PR2 acceptance demo, no desktop app needed)**

```bash
TMP=$(mktemp -d)
# Run the watcher briefly so it composes a Snapshot and writes snapshot.json:
BALANZE_DATA_DIR_OVERRIDE="$TMP" timeout 8 cargo run -q -p balanze_cli -- watch --json >/dev/null 2>&1 || true
ls -la "$TMP/snapshot.json"   # should exist
# Now the statusline reads it for the cross-provider segments:
echo '{"rate_limits":{"five_hour":{"used_percentage":82,"resets_at":4102444800}},"cost":{"total_cost_usd":2.5},"model":{"display_name":"Opus"}}' \
  | BALANZE_DATA_DIR_OVERRIDE="$TMP" NO_COLOR=1 cargo run -q -p balanze_cli -- statusline
# Expect the line to now include the Codex %/OpenAI $ segments IF those providers
# are configured on the dev machine; otherwise those cells are absent (None) -
# which still proves the read path (no parse error, no blank).
```

- [ ] **Step 6: Commit** - `feat(statusline): read snapshot.json for cross-provider Codex/OpenAI segments`

---

### Task 5: Document the new IPC artifact

**Goal:** Document `snapshot.json` in `ARCHITECTURE.md` and add the missing `statusline_render` crate to the crate map.

**Files:**
- Modify: `docs/ARCHITECTURE.md` (crate map + IPC contract / boundary #12 area)

**Acceptance Criteria:**
- [ ] The crate map lists `statusline_render` (PR1 omitted it).
- [ ] The IPC section documents `snapshot.json` (writer = the coordinator-owning host via `SnapshotFileSink`; reader = `balanze-cli statusline`), noting it flows opposite to `statusline.snapshot.json`, the 120s freshness window, and the zero-network read invariant.

**Verify:** prose review; no em-dash / Unicode ellipsis introduced; the two IPC files are clearly distinguished.

**Steps:**

- [ ] **Step 1:** Add `statusline_render` to the `ARCHITECTURE.md` crate map with a one-line role ("renders the statusline: segment model, layout, threshold coloring; reads `settings::StatuslineConfig`").

- [ ] **Step 2:** Near boundary #12 (the existing `statusline.snapshot.json` description), add the `snapshot.json` artifact: full `Snapshot` + `captured_at` + `schema_version`; written atomically by `state_coordinator::SnapshotFileSink` from whichever host owns the coordinator (Tauri app / `balanze-cli watch`); read by `balanze-cli statusline` when fresh (within 120s) to fill the Codex/OpenAI segments; statusline does no network in this path (Claude segments come from stdin). Note the two files flow in opposite directions.

- [ ] **Step 3: Commit** - `docs(architecture): document the snapshot.json IPC artifact + statusline_render crate`

---

## Self-Review

**Spec coverage (PR2 scope):**
- Fresh-read path (spec §5.1) -> Tasks 1-4.
- IPC artifact `snapshot.json`, writer=host/reader=statusline (spec §7) -> Tasks 1-3 + 5.
- Zero statusline-initiated network (spec §5.4) -> Task 4 reads a file only; no provider calls.
- Staleness marker wired from the freshness window -> Task 4 `stale`, rendered by PR1's `codex`/`openai_cost` segments.
- Out of PR2: self-compose + per-turn cache (PR3), replace flow (PR4), Codex preset + CHANGELOG/version (PR5).

**Placeholder scan:** none - full code for the new modules + tests; precise edits for the wiring with line refs.

**Type consistency:** `SnapshotFilePayload`/`SnapshotFileError`/`snapshot_file_path`/`read_snapshot_file`/`atomic_write_snapshot_file` defined in Task 1, consumed in Tasks 2 + 4. `SnapshotFileSink` defined Task 2, used Task 3. `cross_from_payload`/`statusline_cross_provider`/`render_with(+cross)` defined Task 4. `Snapshot.codex_quota.primary.used_percent: f64` -> `as f32`; `openai.total_micro_usd: i64` verbatim - matches the explored types.

**Known follow-ups (not PR2):** the `BALANZE_DATA_DIR_OVERRIDE`/ProjectDirs path logic is now resolved in `state_coordinator` for `snapshot.json`, but the pre-existing triple-duplication for `statusline.snapshot.json` (balanze_cli + 2 watcher tasks) is left as-is (out of scope; standing TODO). The `tauri dev` smoke for the Tauri-host writer is deferred to issue #136; the CLI-`watch` path provides the verifiable end-to-end demo.
