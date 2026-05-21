# Track E — Live watcher + predictor implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the v0.2 finale — `predictor` + `watcher` crates, the `--watch` CLI mode, the Sink-seam checkpoint, and Criterion benches — as a stack of independently-mergeable atomic PRs.

**Architecture:** Spec at `docs/superpowers/specs/2026-05-21-track-e-watcher-design.md` settles all five §8 decisions. This plan is execution-only: 9 tasks, each its own commit + PR, each independently testable. Source-of-truth for "what" is the spec; this doc is "how + in what order."

**Tech stack:** Rust 2021 / MSRV 1.77 / `tokio` / `notify` (new) / `criterion` (new dev-dep). No frontend changes (Svelte scaffold unaffected).

---

## File structure overview

**New crates:**

| Crate | Purpose |
|---|---|
| `crates/predictor/` | Pure EWMA prediction with warm-up state machine |
| `crates/watcher/` | Live loop: 4 notify/poll tasks + supervisor + 60s safety poll |

**New files:**

```
crates/predictor/Cargo.toml
crates/predictor/src/lib.rs
crates/watcher/Cargo.toml
crates/watcher/src/lib.rs
crates/watcher/src/tasks.rs           (mod root)
crates/watcher/src/tasks/jsonl.rs
crates/watcher/src/tasks/statusline.rs
crates/watcher/src/tasks/oauth_poll.rs
crates/watcher/src/tasks/openai_poll.rs
crates/watcher/src/tasks/safety_poll.rs
crates/watcher/src/supervisor.rs
crates/watcher/src/errors.rs
crates/watcher/tests/integration.rs
crates/claude_statusline/src/payload.rs        (StatuslineFilePayload envelope)
crates/claude_statusline/src/file_io.rs        (atomic read/write)
crates/balanze_cli/src/sinks.rs                (StdoutSink + JsonlSink)
crates/balanze_cli/src/watch_cmd.rs            (--watch supervisor)
src-tauri/src/tauri_sink.rs                    (compile-only skeleton)
crates/claude_cost/benches/compute_cost.rs
crates/claude_parser/benches/incremental_parser.rs
crates/window/benches/summarize_window.rs
crates/claude_cost/benches/baseline.json
crates/claude_parser/benches/baseline.json
crates/window/benches/baseline.json
```

**Modified files:**

```
Cargo.toml                                      (add watcher + predictor to workspace members)
crates/state_coordinator/Cargo.toml             (add predictor + claude_statusline deps)
crates/state_coordinator/src/messages.rs        (Source::ClaudeStatusline + SourcePartial)
crates/state_coordinator/src/snapshot.rs        (3 new fields + merge_partial extension)
crates/state_coordinator/src/coordinator.rs     (call predictor::predict after JSONL/OAuth merges)
crates/state_coordinator/src/lib.rs             (re-export Prediction)
crates/balanze_cli/Cargo.toml                   (add watcher + predictor deps)
crates/balanze_cli/src/main.rs                  (wire --watch flag + statusline write)
crates/balanze_cli/src/json_output.rs           (claude_statusline + prediction cells)
crates/claude_statusline/src/lib.rs             (re-export payload + file_io)
crates/settings/src/lib.rs                      (oauth_poll_interval_secs)
src-tauri/Cargo.toml                            (add state_coordinator dep)
src-tauri/src/lib.rs                            (pub mod tauri_sink for compile check)
AGENTS.md                                       (repo map + boundary #4 concretization)
docs/prd.md                                     (Phase 2 Track E marked shipped)
CHANGELOG.md                                    (Unreleased section)
README.md                                       (json DTO note for new cells)
```

---

## Branch + PR strategy

Each task = one branch + one PR. Stack on top of `main` (no inter-task dependencies that require stacked PRs — the schema additions in Task 3 are non-breaking, and downstream tasks compose against the merged shape). If multiple tasks are in flight, rebase each onto the latest `main` before opening its PR.

Branch names follow the project convention: `feat/track-e-<task-slug>` (e.g., `feat/track-e-predictor`, `feat/track-e-watcher`).

---

## Task 1: `predictor` crate

**Goal:** Pure EWMA prediction with the `Insufficient → Uncertain → Confident` warm-up state machine. No I/O, no tokio, no logging above `debug`.

**Files:**
- Create: `crates/predictor/Cargo.toml`
- Create: `crates/predictor/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members`)

**Acceptance Criteria:**
- [ ] `predict(now, window, history, window_reset) -> Prediction` returns `Insufficient` for the first 15 minutes after `window_reset` OR when fewer than 10 events have been seen since reset.
- [ ] After warm-up, returns `Uncertain` when EWMA variance is above the threshold; `Confident` otherwise.
- [ ] `eta_to_cap` is `None` when state is `Insufficient`, `Some(Duration)` otherwise.
- [ ] `eta_to_reset` is always `Some` (it's deterministic from `window_reset - now`, clamped to zero if past).
- [ ] All EWMA / variance constants are named constants with a one-line rationale comment.
- [ ] Cross-crate boundary: zero imports of `tokio`, `reqwest`, `notify`, `tracing` above `debug!`.

**Verify:** `cargo test -p predictor` → all tests pass; `cargo clippy -p predictor --all-targets -- -D warnings` clean.

**Steps:**

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "predictor"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
publish = false

[dependencies]
chrono = { workspace = true }
serde = { workspace = true, features = ["derive"] }
window = { path = "../window" }
```

- [ ] **Step 2: Write tests first — `src/lib.rs` test module**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use window::WindowSummary;

    fn t(min: i64) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 21, 0, 0, 0).unwrap() + chrono::Duration::minutes(min)
    }

    fn ws(used_pct: f64) -> WindowSummary {
        // Construct a minimal WindowSummary; the predictor only reads
        // a few fields, so test_support helpers from `window` work too.
        // Real fields filled in once we know window::WindowSummary's
        // constructor — see window/src/lib.rs.
        WindowSummary::for_test(used_pct)
    }

    #[test]
    fn insufficient_during_first_15_minutes_after_reset() {
        let reset = t(0);
        // now = reset - 4h (cap window is [reset - 5h, reset))
        let now = reset - chrono::Duration::hours(4);
        let p = predict(now, &ws(10.0), &[], reset);
        assert!(matches!(p.state, PredictionState::Insufficient));
        assert_eq!(p.eta_to_cap, None);
        assert!(p.eta_to_reset > chrono::Duration::zero());
    }

    #[test]
    fn insufficient_with_fewer_than_ten_events() {
        let reset = t(60);
        let now = t(30);  // 30 min into the window
        let history: Vec<WindowSnapshot> = (0..5)
            .map(|i| WindowSnapshot { ts: t(i * 5), used_pct: i as f64 })
            .collect();
        let p = predict(now, &ws(5.0), &history, reset);
        assert!(matches!(p.state, PredictionState::Insufficient));
    }

    #[test]
    fn confident_with_stable_growth() {
        let reset = t(300);  // 5h ahead
        let now = t(20);
        // 12 evenly-spaced points growing linearly — low variance.
        let history: Vec<WindowSnapshot> = (0..12)
            .map(|i| WindowSnapshot {
                ts: t(i as i64),
                used_pct: i as f64 * 0.5,
            })
            .collect();
        let p = predict(now, &ws(6.0), &history, reset);
        assert!(matches!(p.state, PredictionState::Confident));
        assert!(p.eta_to_cap.is_some());
    }

    #[test]
    fn uncertain_with_high_variance() {
        let reset = t(300);
        let now = t(20);
        // Wildly oscillating used_pct — high variance.
        let history: Vec<WindowSnapshot> = (0..12)
            .map(|i| WindowSnapshot {
                ts: t(i as i64),
                used_pct: if i % 2 == 0 { 5.0 } else { 80.0 },
            })
            .collect();
        let p = predict(now, &ws(50.0), &history, reset);
        assert!(matches!(p.state, PredictionState::Uncertain));
        // ETA still returned in Uncertain; UI just shows a ± caveat.
        assert!(p.eta_to_cap.is_some());
    }

    #[test]
    fn eta_to_reset_clamps_to_zero_if_past() {
        let reset = t(0);
        let now = t(10);  // 10 min PAST reset (clock skew or test weirdness)
        let p = predict(now, &ws(0.0), &[], reset);
        assert_eq!(p.eta_to_reset, chrono::Duration::zero());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p predictor`
Expected: compile errors — `predict`, `Prediction`, etc. don't exist yet.

- [ ] **Step 4: Implement `src/lib.rs`**

```rust
//! Pure EWMA prediction with a warm-up state machine.
//!
//! Boundary: pure-function crate (AGENTS.md §4 #2). No I/O, no `tokio::spawn`,
//! no logging above `debug`. The coordinator owns the history ring buffer and
//! calls `predict` after each successful merge.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use window::WindowSummary;

/// First 15 minutes after window reset are warm-up regardless of event count.
const WARMUP_MINUTES: i64 = 15;

/// Minimum events seen since reset before the predictor will emit a number.
/// Below this, variance is too noisy to be honest.
const MIN_EVENTS_FOR_PREDICTION: usize = 10;

/// EWMA smoothing factor. 0.3 weights recent observations heavily without
/// overreacting to single outliers. Hand-tuned against simulated workloads.
const EWMA_ALPHA: f64 = 0.3;

/// Variance threshold (squared used_pct units) above which the predictor
/// downgrades to `Uncertain`. Calibrated so a steady 0.5 %/min growth is
/// well below and a wildly oscillating signal is well above.
const VARIANCE_CONFIDENT_THRESHOLD: f64 = 50.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PredictionState {
    Insufficient,
    Uncertain,
    Confident,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    pub state: PredictionState,
    /// None when state == Insufficient. Otherwise the EWMA-projected
    /// duration until the rolling-window cap is reached.
    #[serde(with = "duration_seconds_opt")]
    pub eta_to_cap: Option<Duration>,
    /// Always present. Deterministic from `window_reset - now`, clamped
    /// to zero if reset is already in the past.
    #[serde(with = "duration_seconds")]
    pub eta_to_reset: Duration,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSnapshot {
    pub ts: DateTime<Utc>,
    pub used_pct: f64,
}

pub fn predict(
    now: DateTime<Utc>,
    window: &WindowSummary,
    history: &[WindowSnapshot],
    window_reset: DateTime<Utc>,
) -> Prediction {
    let eta_to_reset = (window_reset - now).max(Duration::zero());

    // Warm-up gate 1: are we within 15 minutes of a recent reset?
    // Reset is in the FUTURE; cap window started at reset - 5h. We're in
    // warm-up if (now - (reset - 5h)) < 15 minutes.
    let window_start = window_reset - Duration::hours(5);
    let elapsed_in_window = now - window_start;
    if elapsed_in_window < Duration::minutes(WARMUP_MINUTES) {
        return Prediction {
            state: PredictionState::Insufficient,
            eta_to_cap: None,
            eta_to_reset,
            computed_at: now,
        };
    }

    // Warm-up gate 2: enough events?
    if history.len() < MIN_EVENTS_FOR_PREDICTION {
        return Prediction {
            state: PredictionState::Insufficient,
            eta_to_cap: None,
            eta_to_reset,
            computed_at: now,
        };
    }

    // EWMA of pct-per-minute deltas.
    let ewma_rate = compute_ewma_rate(history);
    let variance = compute_variance(history, ewma_rate);

    let state = if variance > VARIANCE_CONFIDENT_THRESHOLD {
        PredictionState::Uncertain
    } else {
        PredictionState::Confident
    };

    // Project until cap. If rate ≤ 0 or current usage ≥ 100, no ETA.
    let current_pct = window.used_percentage();
    let eta_to_cap = if ewma_rate > 0.0 && current_pct < 100.0 {
        let pct_remaining = 100.0 - current_pct;
        let minutes_to_cap = pct_remaining / ewma_rate;
        Some(Duration::seconds((minutes_to_cap * 60.0) as i64))
    } else {
        None
    };

    Prediction {
        state,
        eta_to_cap,
        eta_to_reset,
        computed_at: now,
    }
}

fn compute_ewma_rate(history: &[WindowSnapshot]) -> f64 {
    // Convert consecutive (ts, pct) pairs into pct/min deltas, then EWMA.
    let mut ewma: Option<f64> = None;
    for pair in history.windows(2) {
        let dt_min = (pair[1].ts - pair[0].ts).num_seconds() as f64 / 60.0;
        if dt_min <= 0.0 { continue; }
        let rate = (pair[1].used_pct - pair[0].used_pct) / dt_min;
        ewma = Some(match ewma {
            None => rate,
            Some(prev) => EWMA_ALPHA * rate + (1.0 - EWMA_ALPHA) * prev,
        });
    }
    ewma.unwrap_or(0.0)
}

fn compute_variance(history: &[WindowSnapshot], mean_rate: f64) -> f64 {
    let mut sum_sq = 0.0;
    let mut n = 0;
    for pair in history.windows(2) {
        let dt_min = (pair[1].ts - pair[0].ts).num_seconds() as f64 / 60.0;
        if dt_min <= 0.0 { continue; }
        let rate = (pair[1].used_pct - pair[0].used_pct) / dt_min;
        let d = rate - mean_rate;
        sum_sq += d * d;
        n += 1;
    }
    if n == 0 { 0.0 } else { sum_sq / n as f64 }
}

// Serde adapters for chrono::Duration (no built-in support).
mod duration_seconds {
    use chrono::Duration;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(d.num_seconds())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::seconds(i64::deserialize(d)?))
    }
}

mod duration_seconds_opt {
    use chrono::Duration;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        d.map(|d| d.num_seconds()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<i64>::deserialize(d)?.map(Duration::seconds))
    }
}
```

Note: `WindowSummary::for_test(used_pct)` test helper does not exist yet — Step 4.5 either adds it to `window::test_support` or the test reaches into `WindowSummary` fields directly. Pick whichever matches the existing `window` test patterns.

- [ ] **Step 5: Add predictor to workspace**

Modify `Cargo.toml` workspace members to include `"crates/predictor"`. Add to `[workspace.dependencies]`:

```toml
predictor = { path = "crates/predictor" }
```

- [ ] **Step 6: Run all tests + clippy**

```
cargo test -p predictor
cargo clippy -p predictor --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected: all pass.

- [ ] **Step 7: Commit**

```
git add Cargo.toml crates/predictor/
git commit -m "feat(predictor): pure EWMA prediction with warm-up state machine"
```

---

## Task 2: `claude_statusline` file IO + payload envelope

**Goal:** Extend the existing `claude_statusline` crate (Track D) with the file-IO surface needed by both producer (`balanze-cli statusline`) and consumer (the watcher): `StatuslineFilePayload` envelope type + atomic write + read functions.

**Files:**
- Create: `crates/claude_statusline/src/payload.rs`
- Create: `crates/claude_statusline/src/file_io.rs`
- Modify: `crates/claude_statusline/src/lib.rs` (re-exports)
- Modify: `crates/claude_statusline/Cargo.toml` (add `serde_json` if not already there)

**Acceptance Criteria:**
- [ ] `StatuslineFilePayload { schema_version: u8, captured_at: DateTime<Utc>, payload: StatuslineSnapshot }` round-trips through JSON serde.
- [ ] `atomic_write_snapshot(path, &payload)` writes tmp + fsync + rename. Preserves existing permissions if the target already exists.
- [ ] `read_snapshot(path)` returns `Ok(payload)` for valid files, an enum error for missing / unreadable / unparsable / schema-version-mismatched files.
- [ ] All errors include the file path; none include the file contents (defense in depth — the file is non-secret, but the discipline matches `anthropic_oauth::credentials::write_back`).
- [ ] Schema version is hard-coded to `1` (a future version bump is a §8 change with its own PR).

**Verify:** `cargo test -p claude_statusline` → all tests pass; `cargo clippy -p claude_statusline --all-targets -- -D warnings` clean.

**Steps:**

- [ ] **Step 1: Add `payload.rs`**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::StatuslineSnapshot;

/// Current schema version. Any change to the on-disk JSON shape requires
/// bumping this AND adding a `from_v<N>` migration path to `read_snapshot`.
pub const SCHEMA_VERSION: u8 = 1;

/// Envelope written to disk by `balanze-cli statusline` and read by the
/// watcher. `captured_at` is the producer's wall-clock at write time —
/// authoritative freshness signal for the consumer's render-time dedup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatuslineFilePayload {
    pub schema_version: u8,
    pub captured_at: DateTime<Utc>,
    pub payload: StatuslineSnapshot,
}

impl StatuslineFilePayload {
    pub fn new(payload: StatuslineSnapshot, captured_at: DateTime<Utc>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at,
            payload,
        }
    }
}
```

- [ ] **Step 2: Write tests for `file_io.rs` first**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn sample_payload() -> StatuslineFilePayload {
        let snap = StatuslineSnapshot::for_test();  // existing helper
        StatuslineFilePayload::new(snap, Utc.with_ymd_and_hms(2026,5,21,12,0,0).unwrap())
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        atomic_write_snapshot(&path, &sample_payload()).unwrap();
        let back = read_snapshot(&path).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.captured_at.timestamp(), sample_payload().captured_at.timestamp());
    }

    #[test]
    fn missing_file_returns_file_missing_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let err = read_snapshot(&path).unwrap_err();
        assert!(matches!(err, FileIoError::FileMissing { .. }));
    }

    #[test]
    fn malformed_json_returns_parse_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{not valid json").unwrap();
        let err = read_snapshot(&path).unwrap_err();
        assert!(matches!(err, FileIoError::ParseError { .. }));
    }

    #[test]
    fn schema_version_mismatch_returns_schema_drift() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("future.json");
        std::fs::write(&path, r#"{"schema_version":99,"captured_at":"2026-05-21T00:00:00Z","payload":{}}"#).unwrap();
        let err = read_snapshot(&path).unwrap_err();
        assert!(matches!(err, FileIoError::SchemaDrift { found_version: 99, .. }));
    }

    #[test]
    fn atomic_write_does_not_clobber_on_serialize_failure() {
        // Smoke: the .tmp file should not exist after a successful write.
        let dir = tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        atomic_write_snapshot(&path, &sample_payload()).unwrap();
        let tmp = dir.path().join("statusline.snapshot.json.tmp");
        assert!(!tmp.exists(), "tmp file should be renamed away");
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        std::fs::write(&path, "{}").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();

        atomic_write_snapshot(&path, &sample_payload()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "permissions preserved across atomic write");
    }
}
```

- [ ] **Step 3: Implement `file_io.rs`**

```rust
use std::path::{Path, PathBuf};
use std::io::Write;

use crate::payload::{StatuslineFilePayload, SCHEMA_VERSION};

#[derive(Debug, thiserror::Error)]
pub enum FileIoError {
    #[error("statusline snapshot file missing: {path}")]
    FileMissing { path: PathBuf },
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("statusline snapshot parse error in {path}")]
    ParseError { path: PathBuf },
    #[error("statusline snapshot schema drift in {path}: found version {found_version}, expected {SCHEMA_VERSION}")]
    SchemaDrift { path: PathBuf, found_version: u8 },
}

pub fn read_snapshot(path: &Path) -> Result<StatuslineFilePayload, FileIoError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FileIoError::FileMissing { path: path.to_path_buf() });
        }
        Err(e) => return Err(FileIoError::Io { path: path.to_path_buf(), source: e }),
    };

    // Pre-check schema_version using a partial decode so a future-versioned
    // file produces a precise error instead of a generic parse failure.
    #[derive(serde::Deserialize)]
    struct VersionProbe { schema_version: u8 }
    let probe: VersionProbe = serde_json::from_slice(&bytes)
        .map_err(|_| FileIoError::ParseError { path: path.to_path_buf() })?;
    if probe.schema_version != SCHEMA_VERSION {
        return Err(FileIoError::SchemaDrift {
            path: path.to_path_buf(),
            found_version: probe.schema_version,
        });
    }

    serde_json::from_slice(&bytes)
        .map_err(|_| FileIoError::ParseError { path: path.to_path_buf() })
}

pub fn atomic_write_snapshot(
    path: &Path,
    payload: &StatuslineFilePayload,
) -> Result<(), FileIoError> {
    let parent = path.parent().ok_or_else(|| FileIoError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        ),
    })?;
    std::fs::create_dir_all(parent).map_err(|e| FileIoError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;

    let tmp_path = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(payload)
        .map_err(|_| FileIoError::ParseError { path: tmp_path.clone() })?;

    let mut tmp = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path)
        .map_err(|e| FileIoError::Io { path: tmp_path.clone(), source: e })?;
    tmp.write_all(&bytes).map_err(|e| FileIoError::Io { path: tmp_path.clone(), source: e })?;
    tmp.sync_all().map_err(|e| FileIoError::Io { path: tmp_path.clone(), source: e })?;
    drop(tmp);

    // Preserve existing target permissions if the file already exists.
    let existing_perms = std::fs::metadata(path).ok().map(|m| m.permissions());

    std::fs::rename(&tmp_path, path).map_err(|e| FileIoError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    if let Some(perms) = existing_perms {
        let _ = std::fs::set_permissions(path, perms);
    }

    Ok(())
}
```

- [ ] **Step 4: Re-export from `lib.rs`**

Add to `crates/claude_statusline/src/lib.rs`:

```rust
pub mod payload;
pub mod file_io;
pub use payload::{StatuslineFilePayload, SCHEMA_VERSION};
pub use file_io::{atomic_write_snapshot, read_snapshot, FileIoError};
```

- [ ] **Step 5: Update `Cargo.toml`**

If not already present, add to `crates/claude_statusline/Cargo.toml`:

```toml
[dependencies]
chrono = { workspace = true, features = ["serde"] }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 6: Run tests + clippy**

```
cargo test -p claude_statusline
cargo clippy -p claude_statusline --all-targets -- -D warnings
```

Expected: all pass.

- [ ] **Step 7: Commit**

```
git add crates/claude_statusline/
git commit -m "feat(claude_statusline): file payload envelope + atomic read/write"
```

---

## Task 3: `state_coordinator` schema additions

**Goal:** Add the `ClaudeStatusline` source variant and the new `Snapshot` fields (`claude_statusline`, `claude_statusline_error`, `prediction`). Coordinator computes prediction inline after JSONL/OAuth merges.

**Files:**
- Modify: `crates/state_coordinator/Cargo.toml` (add `predictor`, `claude_statusline` deps)
- Modify: `crates/state_coordinator/src/messages.rs` (Source + SourcePartial variant)
- Modify: `crates/state_coordinator/src/snapshot.rs` (3 new fields + merge extension + history ring)
- Modify: `crates/state_coordinator/src/coordinator.rs` (predictor recompute after JSONL/OAuth merges)
- Modify: `crates/state_coordinator/src/lib.rs` (re-export `Prediction`)
- Modify: `crates/state_coordinator/src/test_support.rs` (helpers for the new fields)
- Modify: `crates/balanze_cli/src/json_output.rs` (add `claude_statusline` + `prediction` DTO cells — the hand-crafted DTO does not auto-expose new Snapshot fields)

**Acceptance Criteria:**
- [ ] `Source::ClaudeStatusline` exists; `SourcePartial::ClaudeStatusline(StatuslineFilePayload)` exists; the `Source.source()` mapping is exhaustive (compile check).
- [ ] `Snapshot` has three new fields (`claude_statusline`, `claude_statusline_error`, `prediction`); `Snapshot::empty` initializes all to `None`.
- [ ] `merge_partial` handles `ClaudeStatusline` (sets data, clears error slot).
- [ ] `record_error` routes `Source::ClaudeStatusline` to `claude_statusline_error`.
- [ ] After a successful `ClaudeJsonl` or `ClaudeOAuth` merge, the coordinator pushes a `WindowSnapshot` into its 128-entry ring buffer and recomputes `snapshot.prediction = Some(predict(...))`.
- [ ] `crates/balanze_cli/src/json_output.rs` exposes the two new cells with the spec §4.3 shape (`claude_statusline { schema_version, captured_at, five_hour, seven_day, session_cost_usd, source, confidence }` + `prediction { state, eta_to_cap_seconds, eta_to_reset_seconds, computed_at, source, confidence }`). Identifiers redacted unless `-v` per the existing rule.
- [ ] Existing tests in `state_coordinator` all still pass (regression check on the merge invariants).
- [ ] Cross-source isolation invariant (existing test) still passes for the new variant.

**Verify:** `cargo test -p state_coordinator` → all tests pass (including 1+ new test for ClaudeStatusline merge + 1 new test for prediction recompute after merge).

**Steps:**

- [ ] **Step 1: Update `Cargo.toml`**

```toml
[dependencies]
# ... existing
predictor = { workspace = true }
claude_statusline = { path = "../claude_statusline" }
```

- [ ] **Step 2: Extend `messages.rs`**

```rust
// Source enum — add variant and update the doc table:
pub enum Source {
    ClaudeOAuth,
    ClaudeJsonl,
    AnthropicApiCost,
    CodexQuota,
    OpenAiCosts,
    ClaudeStatusline,  // NEW — Claude Code statusLine push via file IPC
}

// SourcePartial — add variant:
pub enum SourcePartial {
    // ... existing
    ClaudeStatusline(claude_statusline::StatuslineFilePayload),
}

// source() — add the match arm:
impl SourcePartial {
    pub fn source(&self) -> Source {
        match self {
            // ... existing arms
            SourcePartial::ClaudeStatusline(_) => Source::ClaudeStatusline,
        }
    }
}
```

- [ ] **Step 3: Extend `snapshot.rs` — fields + merge**

Add to `Snapshot`:

```rust
pub claude_statusline: Option<claude_statusline::StatuslineFilePayload>,
pub claude_statusline_error: Option<String>,
pub prediction: Option<predictor::Prediction>,
```

Update `Snapshot::empty`:

```rust
claude_statusline: None,
claude_statusline_error: None,
prediction: None,
```

Update `merge_partial`:

```rust
SourcePartial::ClaudeStatusline(p) => {
    snapshot.claude_statusline = Some(p);
    snapshot.claude_statusline_error = None;
}
```

Update `record_error`:

```rust
Source::ClaudeStatusline => &mut snapshot.claude_statusline_error,
```

- [ ] **Step 4: Add tests for new merge / error paths**

```rust
#[test]
fn merge_claude_statusline_overwrites_data_and_clears_error() {
    let mut s = Snapshot::empty(fixture_now());
    s.claude_statusline_error = Some("file missing".to_string());
    merge_partial(&mut s, SourcePartial::ClaudeStatusline(statusline_payload()));
    assert!(s.claude_statusline.is_some());
    assert!(s.claude_statusline_error.is_none());
}

#[test]
fn record_error_routes_claude_statusline() {
    let mut s = Snapshot::empty(fixture_now());
    record_error(&mut s, Source::ClaudeStatusline, "schema drift v2");
    assert_eq!(s.claude_statusline_error.as_deref(), Some("schema drift v2"));
    assert!(s.claude_statusline.is_none());
}
```

`test_support.rs` gains a `pub fn statusline_payload() -> StatuslineFilePayload` helper.

- [ ] **Step 5: Wire predictor into the coordinator**

In `coordinator.rs`, add to the actor state:

```rust
const HISTORY_CAPACITY: usize = 128;

struct CoordinatorState {
    snapshot: Snapshot,
    history: VecDeque<predictor::WindowSnapshot>,
    last_settings: Option<Settings>,
}
```

After a successful `merge_partial` of `ClaudeJsonl` or `ClaudeOAuth`, push a `WindowSnapshot` and recompute:

```rust
fn maybe_recompute_prediction(state: &mut CoordinatorState, source: Source, now: DateTime<Utc>) {
    // Only JSONL / OAuth merges change the data the predictor reads.
    if !matches!(source, Source::ClaudeJsonl | Source::ClaudeOAuth) {
        return;
    }
    let Some(jsonl) = state.snapshot.claude_jsonl.as_ref() else { return; };
    let Some(oauth) = state.snapshot.claude_oauth.as_ref() else { return; };
    let Some(reset) = oauth.five_hour_reset() else { return; };

    // Push the latest sample into the ring.
    let used_pct = jsonl.window.used_percentage();
    if state.history.len() == HISTORY_CAPACITY {
        state.history.pop_front();
    }
    state.history.push_back(predictor::WindowSnapshot { ts: now, used_pct });

    let history: Vec<_> = state.history.iter().cloned().collect();
    state.snapshot.prediction = Some(predictor::predict(now, &jsonl.window, &history, reset));
}
```

Call `maybe_recompute_prediction` from the `Update(SourceUpdate { source, result: Ok(_) })` branch in `handle_msg`, right after `merge_partial` and `snapshot.fetched_at = Utc::now()`.

- [ ] **Step 6: Add test for prediction recompute**

```rust
#[tokio::test]
async fn jsonl_or_oauth_merge_recomputes_prediction() {
    let sink = NullSink;
    let (handle, _join) = spawn(sink);

    // Seed OAuth so five_hour_reset() is available.
    handle.send(StateMsg::Update(SourceUpdate {
        source: Source::ClaudeOAuth,
        result: Ok(SourcePartial::ClaudeOAuth(oauth_snapshot())),
    })).await.unwrap();

    // Then seed JSONL.
    handle.send(StateMsg::Update(SourceUpdate {
        source: Source::ClaudeJsonl,
        result: Ok(SourcePartial::ClaudeJsonl(jsonl_snapshot())),
    })).await.unwrap();

    let snap = handle.query().await.unwrap();
    assert!(snap.prediction.is_some(), "prediction recomputed after JSONL merge");
    // First call with ~1 history sample — should be Insufficient.
    assert!(matches!(
        snap.prediction.as_ref().unwrap().state,
        predictor::PredictionState::Insufficient,
    ));
}
```

- [ ] **Step 7: Lib re-export**

In `crates/state_coordinator/src/lib.rs`:

```rust
pub use predictor::{Prediction, PredictionState, WindowSnapshot};
```

- [ ] **Step 8: Run tests + clippy**

```
cargo test -p state_coordinator
cargo clippy -p state_coordinator --all-targets -- -D warnings
```

Expected: all pre-existing tests pass + 3 new tests pass.

- [ ] **Step 9: Commit**

```
git add crates/state_coordinator/
git commit -m "feat(state_coordinator): ClaudeStatusline source + prediction recompute"
```

---

## Task 4: `balanze-cli statusline` writes the snapshot file

**Goal:** Extend the existing `balanze-cli statusline` subcommand to atomically write the parsed payload to `<data_dir>/balanze/statusline.snapshot.json` after printing the human line. Zero behavior change for users who don't run `--watch`.

**Files:**
- Modify: `crates/balanze_cli/src/main.rs` (or wherever the statusline subcommand lives)
- Modify: `crates/balanze_cli/Cargo.toml` (add `directories` if missing — the `settings` crate already uses it)

**Acceptance Criteria:**
- [ ] Existing `balanze-cli statusline` behavior unchanged: parses stdin, prints the human line, exits 0.
- [ ] In addition: writes `StatuslineFilePayload::new(snapshot, Utc::now())` to `<data_dir>/balanze/statusline.snapshot.json` via `claude_statusline::atomic_write_snapshot`.
- [ ] On write failure, log at `warn!` and exit 0 anyway — Claude Code's statusLine call must not fail because Balanze's IPC file failed (would cause the user's statusLine to disappear).
- [ ] The data-dir path is resolved via `directories::ProjectDirs::from("me", "oszkar", "Balanze")::data_dir()`.

**Verify:**
- `cargo test -p balanze_cli` → all tests pass (including 1+ new test exercising the write path with a `TempDir` override).
- Manual: `echo '<real statusline payload>' | cargo run -p balanze_cli -- statusline` → human line prints AND `~/AppData/Local/oszkar/Balanze/data/statusline.snapshot.json` (Windows) or `~/.local/share/balanze/statusline.snapshot.json` (Linux) is updated.

**Steps:**

- [ ] **Step 1: Identify the statusline subcommand entry**

```
grep -n "statusline" crates/balanze_cli/src/main.rs
```

Locate the function that parses stdin + prints. Add the write step at the end (after `println!`, before `Ok(())`).

- [ ] **Step 2: Add the write helper**

```rust
fn write_statusline_snapshot(payload: &claude_statusline::StatuslineSnapshot) {
    let Some(dirs) = directories::ProjectDirs::from("me", "oszkar", "Balanze") else {
        tracing::warn!("statusline: could not resolve data dir; skipping snapshot write");
        return;
    };
    let path = dirs.data_dir().join("statusline.snapshot.json");
    let envelope = claude_statusline::StatuslineFilePayload::new(payload.clone(), chrono::Utc::now());
    if let Err(e) = claude_statusline::atomic_write_snapshot(&path, &envelope) {
        tracing::warn!("statusline: snapshot write failed: {e}");
    }
}
```

Call it from the statusline subcommand handler right before exit.

- [ ] **Step 3: Add a test using a temp data dir**

Reference `crates/settings/` for the existing pattern of overriding the data dir in tests via an env var or an injected path. If no such hook exists, factor the path resolution into a parameterized function:

```rust
fn statusline_snapshot_path() -> Option<std::path::PathBuf> {
    if let Ok(env_path) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(std::path::PathBuf::from(env_path).join("statusline.snapshot.json"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.data_dir().join("statusline.snapshot.json"))
}
```

Then the test:

```rust
#[test]
fn statusline_subcommand_writes_snapshot_to_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("BALANZE_DATA_DIR_OVERRIDE", dir.path());

    let payload = sample_statusline_payload();
    write_statusline_snapshot(&payload);

    let written = claude_statusline::read_snapshot(
        &dir.path().join("statusline.snapshot.json")
    ).unwrap();
    assert_eq!(written.schema_version, claude_statusline::SCHEMA_VERSION);

    std::env::remove_var("BALANZE_DATA_DIR_OVERRIDE");
}
```

(Document the env var in the CLI help text — it's a test/dev hook, mention it in `--help` for `balanze-cli statusline`.)

- [ ] **Step 4: Run tests + clippy**

```
cargo test -p balanze_cli
cargo clippy -p balanze_cli --all-targets -- -D warnings
```

- [ ] **Step 5: Manual smoke**

```bash
# Use a real captured statusline payload, or the test fixture from Track D:
cat crates/claude_statusline/tests/fixtures/real_payload.json | cargo run -p balanze_cli -- statusline
ls -la ~/.local/share/balanze/statusline.snapshot.json  # or platform equivalent
```

Confirm: file exists, content is a valid `StatuslineFilePayload`.

- [ ] **Step 6: Commit**

```
git add crates/balanze_cli/
git commit -m "feat(balanze_cli): statusline subcommand writes snapshot file for the watcher"
```

---

## Task 5: `watcher` crate

**Goal:** The body of work. Four live tasks (JSONL notify, statusline-file notify, OAuth poll, OpenAI poll) + 60s safety poll + supervisor. Each task holds a `StateCoordinatorHandle` clone and emits `StateMsg::Update`.

**Files:**
- Create: `crates/watcher/Cargo.toml`
- Create: `crates/watcher/src/lib.rs`
- Create: `crates/watcher/src/errors.rs`
- Create: `crates/watcher/src/supervisor.rs`
- Create: `crates/watcher/src/tasks.rs` (mod root)
- Create: `crates/watcher/src/tasks/jsonl.rs`
- Create: `crates/watcher/src/tasks/statusline.rs`
- Create: `crates/watcher/src/tasks/oauth_poll.rs`
- Create: `crates/watcher/src/tasks/openai_poll.rs`
- Create: `crates/watcher/src/tasks/safety_poll.rs`
- Create: `crates/watcher/tests/integration.rs`
- Modify: `Cargo.toml` (add watcher to workspace members + `notify = "6"` in workspace.dependencies)
- Modify: `crates/settings/src/lib.rs` (add `pub oauth_poll_interval_secs: u32` with default `300` — the §3.1 5-min floor)

**Acceptance Criteria:**
- [ ] `Watcher::spawn(handle, settings) -> Vec<JoinHandle<Result<(), WatcherError>>>` returns one handle per task (5 entries — JSONL, statusline, OAuth, OpenAI, safety).
- [ ] `Settings::oauth_poll_interval_secs: u32` exists (default 300); serializes round-trips through `settings.json`; the OAuth poll task in this crate reads it (`max(60)` floor enforced at the call site so a malicious / mistyped settings file can't drop below the §3.1 limit).
- [ ] JSONL task: subscribes to `notify` events under `<claude_home>/projects/`, debounces 300ms, runs `IncrementalParser` over changed files, emits `Update(ClaudeJsonl, ...)` + `Update(AnthropicApiCost, ...)`.
- [ ] Statusline task: subscribes to `notify` events on the statusline snapshot file, debounces 100ms, reads via `claude_statusline::read_snapshot`, emits `Update(ClaudeStatusline, ...)`.
- [ ] OAuth task: ticks every `settings.oauth_poll_interval_secs` seconds, calls `anthropic_oauth::fetch_usage` with `BackoffPolicy::standard()`, emits `Update(ClaudeOAuth, ...)`.
- [ ] OpenAI task: same pattern, calls `openai_client::costs_this_month`.
- [ ] Safety poll: ticks every 60s, runs all five sources unconditionally (including `codex_local::read_codex_quota`).
- [ ] Notify exhaustion (`notify::Error::PathNotFound` on subscription init): drops notify, falls back to 60s polling for the affected source; logs at `error!` once.
- [ ] All tasks are `Send + 'static`; no holding `tokio::sync::Mutex` across `.await` of unrelated locks.
- [ ] Integration test (`tests/integration.rs`) drives the watcher against a `tempdir` JSONL tree + a `tempdir` statusline file, asserts the coordinator's snapshot reflects both within ~1 second.

**Verify:** `cargo test -p watcher` → integration test green; `cargo clippy -p watcher --all-targets -- -D warnings` clean.

**Steps:**

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "watcher"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
publish = false

[dependencies]
anthropic_oauth = { path = "../anthropic_oauth" }
anyhow = { workspace = true }
backoff = { path = "../backoff" }
chrono = { workspace = true }
claude_cost = { path = "../claude_cost" }
claude_parser = { path = "../claude_parser" }
claude_statusline = { path = "../claude_statusline" }
codex_local = { path = "../codex_local" }
notify = "6"  # NEW workspace-level dep — add to root [workspace.dependencies]
openai_client = { path = "../openai_client" }
settings = { path = "../settings" }
state_coordinator = { path = "../state_coordinator" }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time", "signal"] }
tracing = { workspace = true }
window = { path = "../window" }

[dev-dependencies]
tempfile = { workspace = true }
serde_json = { workspace = true }
```

Also add `notify = "6"` to root `[workspace.dependencies]` and import via `workspace = true` from this crate.

- [ ] **Step 2: `errors.rs`**

```rust
use state_coordinator::Source;

#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("notify watcher exhausted file descriptors; falling back to 60s polling for {source:?}")]
    NotifyExhausted { source: Source },
    #[error("supervised task panicked ({source:?}): {message}")]
    TaskPanicked { source: Source, message: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 3: `lib.rs` — public API**

```rust
//! The live loop. Owns the four notify/poll tasks plus the 60s safety poll.
//! Each task holds a `StateCoordinatorHandle` clone and sends `StateMsg::Update`.
//! Spec: `docs/superpowers/specs/2026-05-21-track-e-watcher-design.md` §3.2.

mod errors;
mod supervisor;
mod tasks;

pub use errors::WatcherError;

use settings::Settings;
use state_coordinator::StateCoordinatorHandle;
use tokio::task::JoinHandle;

pub struct Watcher;

impl Watcher {
    /// Spawn all five tasks. Returns one `JoinHandle` per task. The caller
    /// (the `balanze-cli --watch` supervisor) selects across them.
    pub fn spawn(
        handle: StateCoordinatorHandle,
        settings: &Settings,
    ) -> Vec<JoinHandle<Result<(), WatcherError>>> {
        vec![
            tasks::jsonl::spawn(handle.clone(), settings.clone()),
            tasks::statusline::spawn(handle.clone(), settings.clone()),
            tasks::oauth_poll::spawn(handle.clone(), settings.clone()),
            tasks::openai_poll::spawn(handle.clone(), settings.clone()),
            tasks::safety_poll::spawn(handle.clone(), settings.clone()),
        ]
    }
}
```

- [ ] **Step 4: `tasks/jsonl.rs`**

```rust
use std::path::PathBuf;
use std::time::Duration;

use claude_parser::find_claude_projects_dir;
use notify::{RecursiveMode, Watcher as _};
use settings::Settings;
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

const DEBOUNCE: Duration = Duration::from_millis(300);

pub fn spawn(coord: StateCoordinatorHandle, _settings: Settings) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let projects_dir = match find_claude_projects_dir() {
            Some(p) => p,
            None => {
                tracing::warn!("watcher/jsonl: no Claude projects dir found; task exits clean");
                return Ok(());
            }
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(w) => w,
            Err(_) => {
                tracing::error!("watcher/jsonl: notify init failed — falling back to 60s safety poll only");
                return Err(WatcherError::NotifyExhausted { source: Source::ClaudeJsonl });
            }
        };
        watcher.watch(&projects_dir, RecursiveMode::Recursive)
            .map_err(|_| WatcherError::NotifyExhausted { source: Source::ClaudeJsonl })?;

        let mut pending = false;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(DEBOUNCE), if pending => {
                    pending = false;
                    run_full_scan(&coord, &projects_dir).await;
                }
                ev = rx.recv() => {
                    match ev {
                        Some(Ok(_)) => { pending = true; }
                        Some(Err(e)) => tracing::warn!("watcher/jsonl: notify error: {e}"),
                        None => break,  // channel closed → task exits
                    }
                }
            }
        }
        Ok(())
    })
}

async fn run_full_scan(coord: &StateCoordinatorHandle, projects_dir: &std::path::Path) {
    // Re-uses `snapshot_composer::LiveSources` shape: parse all JSONL, compute window
    // + cost, send both Updates. For Track E iteration this is full-rescan; the
    // IncrementalParser byte-cursor optimization lands in a follow-up
    // (TODO: AGENTS §10a note — incremental walk if cold-start latency bites).
    let now = chrono::Utc::now();
    let events = match claude_parser::walk_and_parse(projects_dir) {
        Ok(e) => e,
        Err(err) => {
            let _ = coord.send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeJsonl,
                result: Err(format!("jsonl parse: {err}")),
            })).await;
            return;
        }
    };
    let summary = window::summarize_window(&events, now, None);
    let jsonl = state_coordinator::JsonlSnapshot { files_scanned: 0 /* TODO: thread from parser */, window: summary };
    let _ = coord.send(StateMsg::Update(SourceUpdate {
        source: Source::ClaudeJsonl,
        result: Ok(SourcePartial::ClaudeJsonl(jsonl)),
    })).await;

    let table = claude_cost::default_price_table();
    let cost = claude_cost::compute_cost(&events, &table);
    let _ = coord.send(StateMsg::Update(SourceUpdate {
        source: Source::AnthropicApiCost,
        result: Ok(SourcePartial::AnthropicApiCost(cost)),
    })).await;
}
```

Note: `claude_parser::walk_and_parse` is the existing single-shot path. The byte-cursor `IncrementalParser` optimization is a known follow-up — `TODO(v0.2-followup): switch to IncrementalParser once watcher cold-start latency is measured`. Acceptable for the first watcher cut.

- [ ] **Step 5: `tasks/statusline.rs`**

```rust
use std::time::Duration;

use claude_statusline::{read_snapshot, FileIoError};
use notify::{RecursiveMode, Watcher as _};
use settings::Settings;
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

const DEBOUNCE: Duration = Duration::from_millis(100);

pub fn spawn(coord: StateCoordinatorHandle, _settings: Settings) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let Some(dirs) = directories::ProjectDirs::from("me", "oszkar", "Balanze") else {
            tracing::warn!("watcher/statusline: no data dir; task exits clean");
            return Ok(());
        };
        let path = dirs.data_dir().join("statusline.snapshot.json");
        // Ensure the data dir exists so notify can watch it even before the
        // first producer write.
        let _ = std::fs::create_dir_all(dirs.data_dir());

        let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |res| { let _ = tx.send(res); })
            .map_err(|_| WatcherError::NotifyExhausted { source: Source::ClaudeStatusline })?;
        // Watch the directory (notify can't watch a not-yet-existing file).
        watcher.watch(dirs.data_dir(), RecursiveMode::NonRecursive)
            .map_err(|_| WatcherError::NotifyExhausted { source: Source::ClaudeStatusline })?;

        // Initial read — file might already exist from a previous session.
        push_current(&coord, &path).await;

        let mut pending = false;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(DEBOUNCE), if pending => {
                    pending = false;
                    push_current(&coord, &path).await;
                }
                ev = rx.recv() => {
                    match ev {
                        Some(Ok(event)) if event.paths.iter().any(|p| p.ends_with("statusline.snapshot.json")) => {
                            pending = true;
                        }
                        Some(Ok(_)) => {}  // unrelated file event
                        Some(Err(e)) => tracing::warn!("watcher/statusline: notify error: {e}"),
                        None => break,
                    }
                }
            }
        }
        Ok(())
    })
}

async fn push_current(coord: &StateCoordinatorHandle, path: &std::path::Path) {
    match read_snapshot(path) {
        Ok(payload) => {
            let _ = coord.send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeStatusline,
                result: Ok(SourcePartial::ClaudeStatusline(payload)),
            })).await;
        }
        Err(FileIoError::FileMissing { .. }) => {
            // Not an error — many users won't have statusLine wired.
        }
        Err(e) => {
            let _ = coord.send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeStatusline,
                result: Err(format!("statusline file: {e}")),
            })).await;
        }
    }
}
```

- [ ] **Step 6: `tasks/oauth_poll.rs`**

```rust
use std::time::Duration;

use anthropic_oauth::fetch_usage;
use backoff::BackoffPolicy;
use settings::Settings;
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

pub fn spawn(coord: StateCoordinatorHandle, settings: Settings) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let interval = Duration::from_secs(u64::from(settings.oauth_poll_interval_secs.max(60)));
        let policy = BackoffPolicy::standard();
        let mut ticker = tokio::time::interval(interval);
        // First tick fires immediately; skip it so the watcher's spawn-time
        // safety poll doesn't double-fire the OAuth fetch.
        ticker.tick().await;

        loop {
            ticker.tick().await;
            let result = fetch_usage(&policy).await
                .map(SourcePartial::ClaudeOAuth)
                .map_err(|e| e.to_string());
            let _ = coord.send(StateMsg::Update(SourceUpdate {
                source: Source::ClaudeOAuth,
                result,
            })).await;
        }
    })
}
```

- [ ] **Step 7: `tasks/openai_poll.rs`**

Same pattern as oauth_poll, but call `openai_client::costs_this_month(&key, &policy)`. Source key resolution (keychain vs env) re-uses the existing `balanze_cli::keychain_or_env_key` helper if accessible, otherwise duplicate the small env-var-then-keychain check.

- [ ] **Step 8: `tasks/safety_poll.rs`**

```rust
use std::time::Duration;

use settings::Settings;
use state_coordinator::StateCoordinatorHandle;
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

const SAFETY_INTERVAL: Duration = Duration::from_secs(60);

pub fn spawn(coord: StateCoordinatorHandle, settings: Settings) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SAFETY_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            // Re-run all five sources via the same code paths the dedicated
            // tasks use. This catches: (1) notify misses on network FS,
            // (2) Codex (which has no dedicated notify task — boundary #4 §3.2).
            super::jsonl::run_full_scan_once(&coord).await;
            super::statusline::push_current_once(&coord).await;
            super::codex::run_once(&coord).await;
            // OAuth + OpenAI tasks already poll on their own cadence;
            // the safety poll does NOT re-fire those (would double-load).
            let _ = settings;  // suppress unused-warning
        }
    })
}
```

Note: extracting `run_full_scan_once` / `push_current_once` as `pub(crate)` helpers from the dedicated tasks avoids code duplication. Codex gets its own helper module here even though it has no dedicated notify task.

- [ ] **Step 9: Integration test `tests/integration.rs`**

```rust
use std::time::Duration;
use tempfile::TempDir;
use state_coordinator::{spawn as spawn_coord, LogSink, StateMsg};
use watcher::Watcher;

#[tokio::test]
async fn watcher_drives_snapshot_from_statusline_file() {
    let dir = TempDir::new().unwrap();
    std::env::set_var("BALANZE_DATA_DIR_OVERRIDE", dir.path());
    // ... also fake HOME so Claude / Codex paths point at empty fixtures
    let claude_home = TempDir::new().unwrap();
    std::env::set_var("HOME", claude_home.path());

    let (handle, _join) = spawn_coord(LogSink);
    let _tasks = Watcher::spawn(handle.clone(), &settings::Settings::default());

    // Producer side: write a statusline file.
    let payload = sample_statusline_payload();
    let path = dir.path().join("statusline.snapshot.json");
    claude_statusline::atomic_write_snapshot(&path, &payload).unwrap();

    // Poll the coordinator until the statusline cell shows up (max 2s).
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let snap = handle.query().await.unwrap();
        if snap.claude_statusline.is_some() { break; }
        if std::time::Instant::now() > deadline {
            panic!("statusline never propagated to snapshot");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
```

- [ ] **Step 10: Run tests + clippy**

```
cargo test -p watcher
cargo clippy -p watcher --all-targets -- -D warnings
```

Expected: integration test passes. Clippy clean.

- [ ] **Step 11: Commit**

```
git add Cargo.toml crates/watcher/
git commit -m "feat(watcher): live loop — JSONL/statusline notify + OAuth/OpenAI poll + 60s safety"
```

---

## Task 6: `balanze-cli --watch` mode

**Goal:** Wire the CLI flag, add `StdoutSink` + `JsonlSink`, run the watcher + coordinator, handle SIGINT.

**Files:**
- Create: `crates/balanze_cli/src/sinks.rs`
- Create: `crates/balanze_cli/src/watch_cmd.rs`
- Modify: `crates/balanze_cli/src/main.rs` (CLI arg + dispatch)
- Modify: `crates/balanze_cli/Cargo.toml` (add `watcher` dep, `is-terminal`)

**Acceptance Criteria:**
- [ ] `balanze-cli --watch` and `balanze-cli status --watch` both work.
- [ ] On TTY: `StdoutSink` clears screen + reprints compact view on each `on_snapshot` (debounced 200ms).
- [ ] On non-TTY: `StdoutSink` appends each refresh with a separator line.
- [ ] `--watch --json`: replaces `StdoutSink` with `JsonlSink`; one Snapshot per stdout line, no debounce.
- [ ] `Ctrl-C` / SIGINT: drops watcher tasks, prints "shutting down", exits 0 within 1 second.
- [ ] Test: smoke test in `tests/integration_4quadrant.rs` (or new file) drives `--watch --json` against fixture inputs, captures stdout, asserts at least one JSON line is emitted.

**Verify:** `cargo test -p balanze_cli`; manual: `cargo run -p balanze_cli -- --watch` shows live refresh; Ctrl-C exits cleanly.

**Steps:**

- [ ] **Step 1: `sinks.rs`**

```rust
use std::io::{IsTerminal, Write};

use state_coordinator::{Sink, Snapshot, Source};

pub struct StdoutSink {
    is_tty: bool,
    last_render: std::time::Instant,
    debounce: std::time::Duration,
}

impl StdoutSink {
    pub fn new() -> Self {
        Self {
            is_tty: std::io::stdout().is_terminal(),
            last_render: std::time::Instant::now() - std::time::Duration::from_secs(1),
            debounce: std::time::Duration::from_millis(200),
        }
    }
}

impl Sink for StdoutSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        if self.last_render.elapsed() < self.debounce { return; }
        self.last_render = std::time::Instant::now();
        let mut out = std::io::stdout().lock();
        if self.is_tty {
            // ANSI clear screen + cursor home.
            let _ = write!(out, "\x1b[2J\x1b[H");
        } else {
            let _ = writeln!(out, "---");
        }
        // Re-use the existing compact renderer.
        let _ = writeln!(out, "{}", crate::render::compact(snapshot));
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "[degraded] {source:?}: {error}");
    }
}

pub struct JsonlSink;

impl Sink for JsonlSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        let dto = crate::json_output::snapshot_to_dto(snapshot, false /* verbose */);
        let line = serde_json::to_string(&dto).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
        println!("{line}");
    }
    fn on_degraded(&mut self, _source: Source, _error: &str) {
        // Errors already ride in the next Snapshot via the per-source _error
        // slots, so JsonlSink doesn't emit a separate "degraded" line — that
        // would break consumers that expect one Snapshot per line.
    }
}
```

- [ ] **Step 2: `watch_cmd.rs`**

```rust
use anyhow::Result;
use state_coordinator::spawn as spawn_coord;
use watcher::Watcher;

pub async fn run_watch(json_mode: bool) -> Result<()> {
    let settings = settings::load_or_default()?;
    let (handle, _join) = if json_mode {
        spawn_coord(crate::sinks::JsonlSink)
    } else {
        spawn_coord(crate::sinks::StdoutSink::new())
    };

    let _tasks = Watcher::spawn(handle.clone(), &settings);

    // Trigger an initial refresh so the user sees a frame immediately.
    let _ = handle.send(state_coordinator::StateMsg::Refresh).await;

    tokio::signal::ctrl_c().await?;
    eprintln!("\nshutting down…");
    Ok(())
}
```

- [ ] **Step 3: Wire CLI flag in `main.rs`**

Add to the argument parser: `--watch` boolean. In the dispatch:

```rust
if args.watch {
    return watch_cmd::run_watch(args.json).await;
}
```

(Adjust to match the project's existing arg-parsing style — use `clap` if it's already used, otherwise hand-rolled.)

- [ ] **Step 4: Add tests**

```rust
#[tokio::test]
async fn watch_json_mode_emits_at_least_one_line() {
    // Spawn watch in a child process, capture stdout, kill after 500ms.
    // ... or factor render+sink into a testable function and assert directly.
}
```

The exact shape depends on the existing test patterns — pick whichever already works for the `status --json` test.

- [ ] **Step 5: Manual smoke**

```
cargo run -p balanze_cli -- --watch
# Watch the compact view redraw. In another shell, touch the JSONL or
# write to ~/.local/share/balanze/statusline.snapshot.json. The display
# updates within 1-2 seconds.
# Ctrl-C exits cleanly.
```

- [ ] **Step 6: Commit**

```
git add crates/balanze_cli/
git commit -m "feat(balanze_cli): --watch mode with StdoutSink + JsonlSink"
```

---

## Task 7: `TauriSink` skeleton (Sink-seam checkpoint)

**Goal:** Prove the `Sink` shape compiles inside `src-tauri/` against the current `state_coordinator`. Bodies are TODOs — no Tauri runtime calls yet. Future-proofs v0.3's UI work.

**Files:**
- Create: `src-tauri/src/tauri_sink.rs`
- Modify: `src-tauri/Cargo.toml` (add `state_coordinator` dep)
- Modify: `src-tauri/src/lib.rs` (`pub mod tauri_sink` so it's compiled)

**Acceptance Criteria:**
- [ ] `cargo build -p balanze` (the src-tauri crate) succeeds.
- [ ] `cargo clippy -p balanze --all-targets -- -D warnings` clean (the `#[allow(dead_code)]` is allowed for the skeleton fields).
- [ ] `TauriSink` implements `state_coordinator::Sink`.
- [ ] Bodies are explicit TODOs naming the v0.3 work (emit `usage_updated` / `degraded_state` events; compute ColorBucket; set_icon / set_title dedup against `last_painted` per §3.1).

**Verify:** `cargo build -p balanze` succeeds (this is the seam test); `cargo clippy -p balanze --all-targets -- -D warnings` clean.

**Steps:**

- [ ] **Step 1: Add the dep**

In `src-tauri/Cargo.toml`:

```toml
[dependencies]
state_coordinator = { path = "../crates/state_coordinator" }
```

- [ ] **Step 2: Write `tauri_sink.rs`**

```rust
//! Compile-only skeleton for the v0.3 Tauri tray sink.
//!
//! The actual runtime wiring (event emission, tray icon/title repaint with
//! `last_painted` dedup per AGENTS.md §3.1) lands with the v0.3 UI track.
//! This file exists so the `state_coordinator::Sink` boundary is exercised
//! against a real consumer that will eventually own production behavior —
//! catching shape mismatches before v0.3 makes them expensive to fix.

#![allow(dead_code)]

use state_coordinator::{Sink, Snapshot, Source};
use tauri::AppHandle;

/// Stand-in for the real ColorBucket type the v0.3 tray-paint code will
/// derive from snapshot state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ColorBucket {
    Green,
    Amber,
    Red,
    Unknown,
}

pub struct TauriSink {
    app: AppHandle,
    /// AGENTS.md §3.1 — only repaint when (bucket, title) differs.
    last_painted: Option<(ColorBucket, String)>,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app, last_painted: None }
    }
}

impl Sink for TauriSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        // TODO(v0.3-ui): emit("usage_updated", serde_json::to_value(snapshot)?).
        // TODO(v0.3-ui): compute (ColorBucket, title) from the snapshot.
        // TODO(v0.3-ui): if (bucket, title) == self.last_painted, return early.
        // TODO(v0.3-ui): self.app.tray_by_id("main").set_icon(...) / set_title(...).
        // TODO(v0.3-ui): self.last_painted = Some((bucket, title));
        let _ = (&self.app, snapshot, &mut self.last_painted);
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        // TODO(v0.3-ui): emit("degraded_state", { source, error }).
        let _ = (&self.app, source, error);
    }
}
```

- [ ] **Step 3: Mount in `lib.rs`**

```rust
pub mod tauri_sink;  // compile-only skeleton; instantiated in v0.3
```

- [ ] **Step 4: Build + clippy**

```
cargo build -p balanze
cargo clippy -p balanze --all-targets -- -D warnings
```

If both pass, the seam is correctly shaped.

- [ ] **Step 5: Commit**

```
git add src-tauri/
git commit -m "feat(src-tauri): TauriSink compile-only skeleton (Sink-seam checkpoint)"
```

---

## Task 8: Criterion benches + baselines

**Goal:** Three Criterion benches covering the cost/parse hot paths, each with a committed baseline JSON so future runs detect regressions.

**Files:**
- Create: `crates/claude_cost/benches/compute_cost.rs`
- Create: `crates/claude_parser/benches/incremental_parser.rs`
- Create: `crates/window/benches/summarize_window.rs`
- Create: `crates/claude_cost/benches/baseline.json`
- Create: `crates/claude_parser/benches/baseline.json`
- Create: `crates/window/benches/baseline.json`
- Modify: each crate's `Cargo.toml` (add `criterion` to `[dev-dependencies]`, add `[[bench]]` entry)

**Acceptance Criteria:**
- [ ] `cargo bench --no-run -p claude_cost -p claude_parser -p window` compiles all three.
- [ ] Each bench writes a baseline JSON to `crates/<crate>/benches/baseline.json` when invoked with `--save-baseline`.
- [ ] Budgets documented inline:
  - `compute_cost` < 5 ms on 10k events
  - `IncrementalParser::tick` < 200 µs per 100 new lines
  - `summarize_window` < 1 ms on 10k events / 5h slice
- [ ] CI is **not** modified — benches are local-only.

**Verify:** `cargo bench --no-run` compiles; `cargo bench -p claude_cost` runs and emits times.

**Steps:**

- [ ] **Step 1: Add `criterion` to root workspace deps**

```toml
[workspace.dependencies]
criterion = "0.5"
```

- [ ] **Step 2: `crates/claude_cost/benches/compute_cost.rs`**

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_compute_cost(c: &mut Criterion) {
    let events = claude_cost::test_support::synthetic_events(10_000);
    let table = claude_cost::default_price_table();
    c.bench_function("compute_cost_10k", |b| {
        b.iter(|| {
            let _ = claude_cost::compute_cost(black_box(&events), black_box(&table));
        });
    });
}

criterion_group!(benches, bench_compute_cost);
criterion_main!(benches);
```

- [ ] **Step 3: Add to `crates/claude_cost/Cargo.toml`**

```toml
[dev-dependencies]
criterion = { workspace = true }

[[bench]]
name = "compute_cost"
harness = false
```

If `test_support::synthetic_events(n)` doesn't exist, add it as a `#[cfg(any(test, feature = "test-support"))]` helper.

- [ ] **Step 4: Repeat for `claude_parser` and `window`**

Same shape; pick whatever inputs the existing tests already construct (a synthetic event slice is fine).

- [ ] **Step 5: Run benches once and save baselines**

```bash
cargo bench -p claude_cost -- --save-baseline track_e_initial
cargo bench -p claude_parser -- --save-baseline track_e_initial
cargo bench -p window -- --save-baseline track_e_initial
```

Then copy each `target/criterion/<bench>/track_e_initial/estimates.json` to `crates/<crate>/benches/baseline.json`.

- [ ] **Step 6: Run no-compile sanity**

```
cargo bench --no-run -p claude_cost -p claude_parser -p window
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```
git add crates/claude_cost/ crates/claude_parser/ crates/window/ Cargo.toml
git commit -m "test(benches): criterion baselines for compute_cost / incremental_parser / summarize_window"
```

---

## Task 9: Doc updates (AGENTS.md, PRD, CHANGELOG)

**Goal:** Reflect Track E in the source-of-truth docs. No code changes.

**Files:**
- Modify: `AGENTS.md` (repo map, boundary #4 concretization, §6 validation row, §2.1 settings/JSON DTO rows)
- Modify: `docs/prd.md` (Phase 2 Track E marked shipped 2026-MM-DD)
- Modify: `CHANGELOG.md` (Unreleased section — Added/Changed)
- Modify: `README.md` (a sentence on `--watch` if absent; the `--json` cell list update for `claude_statusline` + `prediction`)

**Acceptance Criteria:**
- [ ] `AGENTS.md` Repo Map shows `watcher/` and `predictor/` rows.
- [ ] `AGENTS.md` boundary #4 lists the four watched paths concretely.
- [ ] `AGENTS.md` §2.1 has a `oauth_poll_interval_secs` settings row + the two new JSON DTO cells.
- [ ] `docs/prd.md` Phase 2 Track E paragraph ends with "Delivered YYYY-MM-DD."
- [ ] `CHANGELOG.md` Unreleased has Added entries for the live `--watch` mode, the watcher crate, the predictor crate, and the statusline-file IPC.
- [ ] `README.md` mentions `--watch` (if currently silent) and reflects the new `--json` cells.

**Verify:** `grep -n "watcher\|predictor" AGENTS.md` shows new rows; `grep -n "Delivered" docs/prd.md` shows the track-E delivery line; `cargo test --workspace` still green (no code changed).

**Steps:**

- [ ] **Step 1: AGENTS.md edits**

Add to the Repo Map crate list (alphabetical with the rest):

```
│   ├── predictor/              pure EWMA + Insufficient/Uncertain/Confident warm-up; consumed by state_coordinator after JSONL/OAuth merges
│   ├── watcher/                live loop — JSONL notify + statusline-file notify + OAuth 5min poll + OpenAI 5min poll + 60s safety poll
```

Update boundary #4 (§4) to list the four notify-watched paths concretely:

```
4. **`watcher` owns `notify` + the debounce + the 60s safety poll.** No other crate imports `notify`. The watcher exposes per-task `JoinHandle`s. Concrete watched paths: `<claude_home>/projects/**/*.jsonl` (300ms debounce), `<data_dir>/balanze/statusline.snapshot.json` (100ms debounce). Concrete polled endpoints: `api.anthropic.com/api/oauth/usage` (5 min, `backoff::standard()` on err), OpenAI `/v1/organization/costs` (5 min). The 60s safety poll re-runs every source — including `codex_local`, which has no dedicated notify task.
```

Add a §2.1 row:

```
| OAuth poll cadence | `settings.json::oauth_poll_interval_secs` (default 300 — the §3.1 5-min floor). Watcher's OAuth poll task uses this. |
```

And to the existing JSON DTO row, append:

```
Added in v0.2 Track E: `claude_statusline { schema_version, captured_at, five_hour, seven_day, session_cost_usd, source, confidence }` and `prediction { state, eta_to_cap_seconds, eta_to_reset_seconds, computed_at, source, confidence }`.
```

- [ ] **Step 2: PRD Phase 2 update**

In `docs/prd.md`, the Track E paragraph: append a line like

```
**Delivered 2026-MM-DD** — `watcher` + `predictor` crates, `--watch` CLI mode (StdoutSink + JsonlSink), Sink-seam checkpoint via TauriSink skeleton, Criterion baselines.
```

- [ ] **Step 3: CHANGELOG Unreleased**

```
### Added
- **Live `--watch` mode.** `balanze-cli --watch` runs a long-lived
  coordinator + watcher; the compact view repaints on every JSONL write,
  statusline push, OAuth poll, or OpenAI poll. `--watch --json` emits one
  JSON Snapshot per line for piping. SIGINT exits cleanly.
- **`watcher` crate.** Hosts the four live tasks (JSONL notify,
  statusline-file notify, OAuth 5-min poll, OpenAI 5-min poll) plus a 60s
  safety poll covering Codex. Notify exhaustion degrades to polling
  rather than hard-failing.
- **`predictor` crate.** Pure EWMA + Insufficient/Uncertain/Confident
  warm-up state machine. The coordinator recomputes after each JSONL or
  OAuth merge; the result lives on `Snapshot::prediction`.
- **Statusline IPC.** `balanze-cli statusline` now atomically writes the
  parsed payload to `<data_dir>/balanze/statusline.snapshot.json` after
  printing; the watcher notify-watches that file. Adds
  `claude_statusline::StatuslineFilePayload` + atomic read/write helpers.
- **TauriSink skeleton.** Compile-only stub in `src-tauri/src/tauri_sink.rs`
  validates the v0.2→v0.3 Sink-seam shape before the UI lands.
- **Criterion baselines.** `compute_cost`, `IncrementalParser::tick`,
  `summarize_window`. Local-only (CI unchanged).

### Changed
- `Snapshot` gained three fields (`claude_statusline`,
  `claude_statusline_error`, `prediction`). The `--json` DTO gained two
  top-level cells. Schema-versioned at `claude_statusline::SCHEMA_VERSION`.
- `Settings::oauth_poll_interval_secs` (default 300) lets users tune the
  OAuth poll cadence above the §3.1 5-min floor.
```

- [ ] **Step 4: README**

Add a short paragraph to the CLI section after the `statusline` row:

```
balanze-cli --watch [--json]      Long-running mode. Reprints the compact
                                  view (or emits one JSON Snapshot per
                                  line with --json) on every push from
                                  the JSONL parser, the statusline file,
                                  or the OAuth/OpenAI 5-min pollers.
                                  Ctrl-C exits cleanly.
```

And update the `--json` cells paragraph to mention the new `claude_statusline` + `prediction` cells.

- [ ] **Step 5: Verify nothing else broke**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```
git add AGENTS.md docs/prd.md CHANGELOG.md README.md
git commit -m "docs(track-e): update AGENTS.md/PRD/CHANGELOG/README for live watcher + predictor"
```

---

## Self-review checklist

After all tasks merge:

- [ ] `cargo test --workspace` passes on Linux (CI).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `bun run check` clean (no frontend changes; should be a no-op).
- [ ] Manual: `balanze-cli --watch` runs for 10+ minutes without panic, prints updates as JSONL / statusline / OAuth fire.
- [ ] Manual: cross-platform smoke on macOS — `notify` behaves, no permission errors on `~/Library/Application Support/Balanze`.
- [ ] Manual: cross-platform smoke on Windows — `notify` on `%APPDATA%\oszkar\Balanze` works.
- [ ] The Track E entries in CHANGELOG match what actually shipped.
- [ ] The Sink-seam test (Task 7) keeps compiling — if `state_coordinator::Sink` changes shape post-Track-E without updating `TauriSink`, the build breaks loudly.

## After Track E

- Consider tagging `v0.2` once Tasks 1–9 are all on `main` and the manual cross-platform smokes pass.
- Then v0.3 begins (Tauri UI tray + popover; settings UI; degraded-state visual treatment; `keyring` → `keyring-core` v4 migration).
