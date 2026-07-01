# Statusline PR3 - Self-compose fallback + per-turn cache - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When no fresh `snapshot.json` exists, the statusline composes its cross-provider Codex % + OpenAI $ segments itself - Codex from local files, OpenAI from the Admin Costs API behind a transcript-independent, key-fingerprinted, 300s mtime-TTL cache - without ever touching the Anthropic OAuth path.

**Architecture:** The real logic (cache primitive + self-compose orchestration) lives in `statusline_render`, mirroring the existing `snapshot_composer` pattern: a `CrossSources` trait abstracts the two I/O sources so the orchestrator + its once-per-300s gate are unit-tested with a counting fake and the crate stays free of `reqwest`/`openai_client`/`codex_local`/`tokio`. The real I/O adapter (`LiveCrossSources`) lives in `balanze_cli` alongside the existing `LiveSources`, reusing the already-OAuth-free `codex_local` + `openai_client` calls. The sync `statusline` command drives the async OpenAI fetch through a one-shot `tokio::runtime::Runtime` exactly as `export`/`doctor`/`status` already do.

**Tech Stack:** Rust 2024, `tokio` (one-shot runtime), `reqwest` (short-timeout client), `openai_client::costs_this_month`, `codex_local::read_codex_quota`, `directories::ProjectDirs` cache dir, `serde`/`serde_json` cache envelope, `chrono` clock, `wiremock` (integration).

---

## Design decisions locked for this PR (read before starting)

These resolve tensions/gaps in `docs/superpowers/specs/2026-06-30-statusline-design.md` and are binding for the tasks below:

1. **Global, fingerprint-keyed OpenAI cache - NOT transcript-path-keyed.** The spec (§5.3) says "transcript-path-keyed," but OpenAI costs are account-global and §3.1/§10 require the OpenAI fetch be "gated to at most once per 300s." A per-transcript cache would let N concurrent conversations each poll every 5 min (N x the API calls), breaking the machine-wide gate. So there is **one** OpenAI-cost cache file, invalidated by an **OpenAI-key fingerprint** (a stable FNV-1a hash of the resolved key, never the key itself). The fingerprint also cleanly separates distinct keys. Consequence: the transcript path is never needed, so **no `claude_statusline` parser extension is required** in this PR.

2. **Per-cell staleness on `CrossProvider`.** Self-compose reads Codex locally every turn (always current) while OpenAI may be served stale from cache. The PR1/PR2 `CrossProvider.stale: bool` drives the `⚠` marker on *both* segments, which would falsely mark fresh local Codex as stale. Split it into `codex_stale` + `openai_stale` (D6: honest over confidently-wrong). The PR2 snapshot path sets both equal to the snapshot-age check (correct - both cells come from one snapshot).

3. **OpenAI fetch uses a SHORT timeout (3s), `BackoffPolicy::fail_fast()`.** A statusline runs every turn; the watcher's 30s client timeout would freeze the prompt. The 300s cache means the network is hit at most once per 5 min; a 3s cap bounds that worst-case turn.

4. **Stale-while-updating, one-shot semantics.** A statusline process is one-shot (no background refresh). Interpretation: if the cache is fresh (< 300s) use it with no network; if stale/missing and not in negative cooldown, attempt one short fetch - on success persist + show fresh, on failure persist the failure timestamp (negative cooldown) and show the last-known value marked `⚠` (never blank). Negative cooldown = 60s so a failing API is not retried every turn.

5. **Codex is not cached in this PR.** The spec calls Codex caching "optional; local read is cheap." YAGNI - read it directly each turn. Codex is therefore never `openai_stale`-style stale; `codex_stale` is always `false` on the self-compose path.

6. **Source selection precedence** in `statusline_cross_provider()`: fresh `snapshot.json` (age <= 120s) wins (zero network, PR2 path); else self-compose; else, if a *stale* snapshot exists, fall back to it marked stale (never-blank); else `None` (Claude-only).

7. **Politeness invariant (§5.4) - non-negotiable.** Self-compose calls only `codex_local` + `openai_client`. It must NEVER reach `anthropic_oauth` / `snapshot_composer::compose` / `live_fetch_oauth`. `statusline_render` must not depend on `anthropic_oauth`.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `crates/statusline_render/src/cache.rs` (new) | Pure OpenAI-cost cache: envelope, atomic read/write, cache-dir resolver, freshness/cooldown predicates, FNV-1a fingerprint. No network. | T1 |
| `crates/statusline_render/src/render.rs` (modify) | `CrossProvider` -> per-cell `codex_stale`/`openai_stale`; update the two `render_segment` marker sites + tests. | T2 |
| `crates/statusline_render/src/self_compose.rs` (new) | `CrossSources` trait + `self_compose()` orchestrator (Codex direct, OpenAI cache-gated). Counting-fake gating tests. | T3 |
| `crates/statusline_render/src/lib.rs` (modify) | Re-export `cache` items, `CrossSources`, `self_compose`. | T1, T3 |
| `crates/statusline_render/Cargo.toml` (modify) | +`directories`, `serde`, `serde_json` (+ `tempfile` dev-dep). | T1 |
| `crates/balanze_cli/src/sources.rs` (modify) | Extract `resolve_openai_key()`; `LiveCrossSources` (Codex local + short-timeout OpenAI, OAuth-free); `BALANZE_OPENAI_API_BASE` seam. | T4 |
| `crates/balanze_cli/src/statusline.rs` (modify) | Restructure `statusline_cross_provider()` precedence; fingerprint; one-shot runtime. | T5 |
| `crates/balanze_cli/tests/` + docs | Integration test (self-compose end-to-end, gate proof) + ARCHITECTURE/AGENTS/TROUBLESHOOTING. | T6 |

---

### Task 1: `statusline_render::cache` - pure OpenAI-cost cache primitive

**Goal:** A network-free, atomically-persisted, key-fingerprinted OpenAI-cost cache with freshness + negative-cooldown predicates and a `BALANZE_CACHE_DIR_OVERRIDE`-aware path resolver.

**Files:**
- Create: `crates/statusline_render/src/cache.rs`
- Modify: `crates/statusline_render/src/lib.rs` (add `pub mod cache;`)
- Modify: `crates/statusline_render/Cargo.toml` (add deps)

**Acceptance Criteria:**
- [ ] Envelope `OpenAiCostEntry { fingerprint, total_micro_usd: Option<i64>, fetched_at: Option<DateTime<Utc>>, last_failure_at: Option<DateTime<Utc>> }` (de)serializes via serde_json.
- [ ] `read(dir, fingerprint)` returns `None` on missing file, parse error, or fingerprint mismatch; `Some(entry)` only on a matching fingerprint.
- [ ] `is_fresh(entry, now)` is true only when `total_micro_usd.is_some()` AND `fetched_at` is within `OPENAI_TTL_SECS = 300`.
- [ ] `in_cooldown(entry, now)` is true when `last_failure_at` is within `NEGATIVE_COOLDOWN_SECS = 60`.
- [ ] `write_success` writes `{fp, Some(v), Some(now), None}`; `write_failure` preserves any prior `total_micro_usd`/`fetched_at` for the same fp and sets `last_failure_at = now` (creates a value-less entry if none/mismatch). Both are atomic (tmp + rename) and leave no `.tmp`.
- [ ] `cache_dir_path()` returns `BALANZE_CACHE_DIR_OVERRIDE` joined with `statusline` when set, else `ProjectDirs::from("me","oszkar","Balanze").cache_dir().join("statusline")`, else `None`.
- [ ] `key_fingerprint(key: Option<&str>)` is a stable FNV-1a hex of the key (empty string when `None`); never the key itself.

**Verify:** `cargo nextest run -p statusline_render cache` -> all green

**Steps:**

- [ ] **Step 1: Add dependencies** to `crates/statusline_render/Cargo.toml`. Under `[dependencies]` add (use workspace deps, matching how `state_coordinator`/`settings` declare them):

```toml
directories = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
```

Under `[dev-dependencies]` add (create the section if absent):

```toml
tempfile = { workspace = true }
```

Confirm each key exists under `[workspace.dependencies]` in the root `Cargo.toml`; if `tempfile` is not there, add `tempfile = "3"` at workspace level (it is already used by `state_coordinator`, so it should exist).

- [ ] **Step 2: Write the failing tests.** Create `crates/statusline_render/src/cache.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone as _, Utc};
    use tempfile::tempdir;

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap()
    }

    #[test]
    fn read_missing_is_none() {
        let dir = tempdir().unwrap();
        assert!(read(dir.path(), "fp").is_none());
    }

    #[test]
    fn write_success_then_read_roundtrips_and_is_fresh() {
        let dir = tempdir().unwrap();
        write_success(dir.path(), "fp", 4_200_000, t0());
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, Some(4_200_000));
        assert!(is_fresh(&e, t0() + Duration::seconds(299)));
        assert!(!is_fresh(&e, t0() + Duration::seconds(301)));
        // no leftover tmp file
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|d| d.ok())
            .filter(|d| d.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp left");
    }

    #[test]
    fn fingerprint_mismatch_reads_none() {
        let dir = tempdir().unwrap();
        write_success(dir.path(), "fp-a", 1, t0());
        assert!(read(dir.path(), "fp-b").is_none());
    }

    #[test]
    fn write_failure_preserves_value_sets_cooldown() {
        let dir = tempdir().unwrap();
        write_success(dir.path(), "fp", 500, t0());
        write_failure(dir.path(), "fp", t0() + Duration::seconds(400));
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, Some(500), "prior value kept");
        assert!(in_cooldown(&e, t0() + Duration::seconds(401)));
        assert!(!in_cooldown(&e, t0() + Duration::seconds(500)));
        assert!(!is_fresh(&e, t0() + Duration::seconds(401)), "stale by TTL");
    }

    #[test]
    fn write_failure_without_prior_has_no_value() {
        let dir = tempdir().unwrap();
        write_failure(dir.path(), "fp", t0());
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, None);
        assert!(!is_fresh(&e, t0()));
        assert!(in_cooldown(&e, t0()));
    }

    #[test]
    fn fingerprint_is_stable_and_distinguishes_keys() {
        assert_eq!(key_fingerprint(Some("sk-abc")), key_fingerprint(Some("sk-abc")));
        assert_ne!(key_fingerprint(Some("sk-abc")), key_fingerprint(Some("sk-xyz")));
        assert_eq!(key_fingerprint(None), key_fingerprint(Some("")));
        // never the raw key
        assert!(!key_fingerprint(Some("sk-abc")).contains("sk-abc"));
    }

    #[test]
    fn cache_dir_path_honors_override() {
        // serialize env mutation with the process-wide lock pattern if added;
        // here a unique value avoids cross-test races.
        let dir = tempdir().unwrap();
        // SAFETY: single-threaded test; restore after.
        unsafe { std::env::set_var("BALANZE_CACHE_DIR_OVERRIDE", dir.path()) };
        let p = cache_dir_path().expect("path");
        assert_eq!(p, dir.path().join("statusline"));
        unsafe { std::env::remove_var("BALANZE_CACHE_DIR_OVERRIDE") };
    }
}
```

- [ ] **Step 3: Run the tests to confirm they fail.**

Run: `cargo nextest run -p statusline_render cache`
Expected: FAIL - `cannot find function read`, etc.

- [ ] **Step 4: Write the implementation** at the top of `crates/statusline_render/src/cache.rs`:

```rust
//! Pure, network-free cache for the self-composed OpenAI cost figure.
//!
//! One global entry per machine (NOT per transcript): OpenAI costs are
//! account-wide, and AGENTS.md 3.1 requires the billing fetch be gated to at
//! most once per 300s machine-wide. The entry is invalidated by a fingerprint
//! of the resolved OpenAI key, so a key rotation forces a refetch and distinct
//! keys never share a value. The fingerprint is a hash, never the key itself
//! (3.4 secret hygiene). The 300s TTL IS the 3.1 politeness gate; the failure
//! cooldown keeps a broken API from being retried every turn.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// TTL for a cached OpenAI cost figure. This is the AGENTS.md 3.1 5-minute gate.
pub const OPENAI_TTL_SECS: i64 = 300;
/// After a failed fetch, do not retry for this long (avoid hammering a 4xx/5xx).
pub const NEGATIVE_COOLDOWN_SECS: i64 = 60;

const FILE_NAME: &str = "openai-cost.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiCostEntry {
    /// FNV-1a hex of the resolved OpenAI key; a mismatch invalidates the entry.
    pub fingerprint: String,
    /// Last successfully fetched total, micro-USD. `None` if only failures so far.
    pub total_micro_usd: Option<i64>,
    /// When `total_micro_usd` was last fetched successfully.
    pub fetched_at: Option<DateTime<Utc>>,
    /// When the most recent fetch attempt failed (drives the negative cooldown).
    pub last_failure_at: Option<DateTime<Utc>>,
}

/// `<BALANZE_CACHE_DIR_OVERRIDE or ProjectDirs.cache>/statusline`.
pub fn cache_dir_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("BALANZE_CACHE_DIR_OVERRIDE") {
        return Some(PathBuf::from(dir).join("statusline"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.cache_dir().join("statusline"))
}

/// Stable FNV-1a-64 hex of the resolved key (empty string when no key). Never
/// the key itself - this is written to disk, so it must not be reversible.
pub fn key_fingerprint(key: Option<&str>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.unwrap_or("").as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Read the entry iff present, parseable, and its fingerprint matches.
pub fn read(dir: &Path, fingerprint: &str) -> Option<OpenAiCostEntry> {
    let bytes = std::fs::read(dir.join(FILE_NAME)).ok()?;
    let entry: OpenAiCostEntry = serde_json::from_slice(&bytes).ok()?;
    (entry.fingerprint == fingerprint).then_some(entry)
}

pub fn is_fresh(entry: &OpenAiCostEntry, now: DateTime<Utc>) -> bool {
    entry.total_micro_usd.is_some()
        && entry
            .fetched_at
            .is_some_and(|t| now.signed_duration_since(t).num_seconds() < OPENAI_TTL_SECS)
}

pub fn in_cooldown(entry: &OpenAiCostEntry, now: DateTime<Utc>) -> bool {
    entry
        .last_failure_at
        .is_some_and(|t| now.signed_duration_since(t).num_seconds() < NEGATIVE_COOLDOWN_SECS)
}

pub fn write_success(dir: &Path, fingerprint: &str, total_micro_usd: i64, now: DateTime<Utc>) {
    write(
        dir,
        &OpenAiCostEntry {
            fingerprint: fingerprint.to_string(),
            total_micro_usd: Some(total_micro_usd),
            fetched_at: Some(now),
            last_failure_at: None,
        },
    );
}

pub fn write_failure(dir: &Path, fingerprint: &str, now: DateTime<Utc>) {
    let prior = read(dir, fingerprint);
    write(
        dir,
        &OpenAiCostEntry {
            fingerprint: fingerprint.to_string(),
            total_micro_usd: prior.as_ref().and_then(|e| e.total_micro_usd),
            fetched_at: prior.as_ref().and_then(|e| e.fetched_at),
            last_failure_at: Some(now),
        },
    );
}

/// Best-effort atomic write (tmp + rename). Errors are logged at debug and
/// swallowed - a cache write failure must never break the statusline.
fn write(dir: &Path, entry: &OpenAiCostEntry) {
    if let Err(e) = try_write(dir, entry) {
        tracing::debug!("statusline cache write failed: {e}");
    }
}

fn try_write(dir: &Path, entry: &OpenAiCostEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let final_path = dir.join(FILE_NAME);
    let tmp_path = dir.join(format!("{FILE_NAME}.tmp"));
    let bytes = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}
```

Then add `pub mod cache;` to `crates/statusline_render/src/lib.rs`.

- [ ] **Step 5: Run the tests to confirm they pass.**

Run: `cargo nextest run -p statusline_render cache`
Expected: PASS (7 tests). Then `cargo clippy -p statusline_render --all-targets -- -D warnings` clean.

- [ ] **Step 6: Commit.**

```bash
git add crates/statusline_render/src/cache.rs crates/statusline_render/src/lib.rs crates/statusline_render/Cargo.toml Cargo.toml
git commit -m "feat(statusline): pure OpenAI-cost cache with TTL, cooldown, and key fingerprint"
```

---

### Task 2: `CrossProvider` per-cell staleness

**Goal:** Replace `CrossProvider.stale: bool` with independent `codex_stale` + `openai_stale` so the `⚠` marker is honest per segment (local Codex is always current on the self-compose path).

**Files:**
- Modify: `crates/statusline_render/src/render.rs` (struct + two `render_segment` marker sites + tests)
- Modify: `crates/balanze_cli/src/statusline.rs` (`cross_from_payload` + its test)

**Acceptance Criteria:**
- [ ] `CrossProvider` has `codex_stale: bool` and `openai_stale: bool` and no `stale` field.
- [ ] The `codex` segment marker reads `cross.codex_stale`; the `openai_cost` segment marker reads `cross.openai_stale`.
- [ ] `cross_from_payload` sets both `codex_stale` and `openai_stale` to `age > SNAPSHOT_FRESHNESS_SECS`.
- [ ] All pre-existing `statusline_render` and `balanze_cli` statusline tests pass after the field rename.

**Verify:** `cargo nextest run -p statusline_render -p balanze_cli` -> green

**Steps:**

- [ ] **Step 1: Update the struct** in `crates/statusline_render/src/render.rs`. Replace:

```rust
#[derive(Debug, Clone, Default)]
pub struct CrossProvider {
    pub codex_used_percent: Option<f32>,
    pub openai_cost_micro_usd: Option<i64>,
    /// True when this cross-provider data is stale (drives the staleness mark).
    pub stale: bool,
}
```

with:

```rust
#[derive(Debug, Clone, Default)]
pub struct CrossProvider {
    pub codex_used_percent: Option<f32>,
    pub openai_cost_micro_usd: Option<i64>,
    /// True when the Codex figure is stale (e.g. an old snapshot). The
    /// self-compose path reads Codex locally each turn, so it is false there.
    pub codex_stale: bool,
    /// True when the OpenAI figure is stale (old snapshot, or a cached value
    /// served because a fresh fetch failed / is in cooldown).
    pub openai_stale: bool,
}
```

- [ ] **Step 2: Update the two marker sites** in `render_segment`. In the `"codex"` arm change `let mark = if cross.stale { " ⚠" } else { "" };` to `let mark = if cross.codex_stale { " ⚠" } else { "" };`. In the `"openai_cost"` arm change `let mark = if cross.stale { " ⚠" } else { "" };` to `let mark = if cross.openai_stale { " ⚠" } else { "" };`.

- [ ] **Step 3: Fix the `render.rs` test** that constructs a `CrossProvider`. Find the test building `CrossProvider { ... stale: ... }` (e.g. `cross_renders_codex_and_openai_segments`) and replace the `stale:` field with `codex_stale: false, openai_stale: false,` (or the values the test intends). If a test specifically asserts the `⚠` marker, set the relevant per-cell field. Run `cargo nextest run -p statusline_render` and fix any other constructors the compiler flags.

- [ ] **Step 4: Update `cross_from_payload`** in `crates/balanze_cli/src/statusline.rs`. Replace its tail:

```rust
    statusline_render::CrossProvider {
        codex_used_percent: snap
            .codex_quota
            .as_ref()
            .map(|q| q.primary.used_percent as f32),
        openai_cost_micro_usd: snap.openai.as_ref().map(|c| c.total_micro_usd),
        stale: age > SNAPSHOT_FRESHNESS_SECS,
    }
```

with:

```rust
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
```

- [ ] **Step 5: Fix the `cross_from_payload` test.** In `cross_from_payload_maps_cells_and_freshness`, replace any `assert!(cross.stale)` / `assert!(!cross.stale)` with the equivalent assertions on both `cross.codex_stale` and `cross.openai_stale`.

- [ ] **Step 6: Run and commit.**

Run: `cargo nextest run -p statusline_render -p balanze_cli` (PASS), then `cargo clippy -p statusline_render -p balanze_cli --all-targets -- -D warnings`.

```bash
git add crates/statusline_render/src/render.rs crates/balanze_cli/src/statusline.rs
git commit -m "refactor(statusline): per-cell staleness on CrossProvider (codex vs openai)"
```

---

### Task 3: `CrossSources` trait + `self_compose` orchestrator

**Goal:** A network-free orchestrator that builds a `CrossProvider` from a `CrossSources` trait - Codex read directly, OpenAI gated through the Task 1 cache with stale-while-updating + negative cooldown - and whose once-per-300s gate is proven with a counting fake.

**Files:**
- Create: `crates/statusline_render/src/self_compose.rs`
- Modify: `crates/statusline_render/src/lib.rs` (`mod self_compose;` + re-exports)

**Acceptance Criteria:**
- [ ] `pub trait CrossSources` with `async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String>` (Ok(Some)=value, Ok(None)=no key configured, Err=fetch failed) and `fn codex_used_percent(&self) -> Option<f32>`.
- [ ] `pub async fn self_compose<S: CrossSources>(sources, cache_dir, fingerprint, now) -> CrossProvider`.
- [ ] Fresh cache (< 300s): returns cached value, `openai_stale=false`, and the source's OpenAI fetch is NOT called.
- [ ] Stale/missing cache, no cooldown: calls the fetch exactly once; on success caches + `openai_stale=false`; on `Ok(None)` -> `None` cell; on `Err` -> caches the failure + serves the last value marked `openai_stale=true`.
- [ ] Cooldown active: does NOT call the fetch; serves the last value marked stale.
- [ ] Two consecutive `self_compose` calls within 300s issue at most ONE fetch (the §3.1 gate), asserted via a call counter.
- [ ] `codex_used_percent` is taken from the source every call; `codex_stale` is always `false`.

**Verify:** `cargo nextest run -p statusline_render self_compose` -> green

**Steps:**

- [ ] **Step 1: Write the failing tests.** Create `crates/statusline_render/src/self_compose.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache;
    use chrono::{Duration, TimeZone as _, Utc};
    use std::cell::Cell;
    use tempfile::tempdir;

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap()
    }

    struct Fake {
        openai: Result<Option<i64>, String>,
        codex: Option<f32>,
        calls: Cell<u32>,
    }
    impl CrossSources for Fake {
        async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String> {
            self.calls.set(self.calls.get() + 1);
            self.openai.clone()
        }
        fn codex_used_percent(&self) -> Option<f32> {
            self.codex
        }
    }

    #[tokio::test]
    async fn empty_cache_fetches_once_and_caches() {
        let dir = tempdir().unwrap();
        let f = Fake { openai: Ok(Some(4_200_000)), codex: Some(6.0), calls: Cell::new(0) };
        let cp = self_compose(&f, dir.path(), "fp", t0()).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(4_200_000));
        assert_eq!(cp.codex_used_percent, Some(6.0));
        assert!(!cp.openai_stale && !cp.codex_stale);
        assert_eq!(f.calls.get(), 1);
        assert!(cache::read(dir.path(), "fp").is_some(), "value cached");
    }

    #[tokio::test]
    async fn second_call_within_ttl_does_not_refetch() {
        let dir = tempdir().unwrap();
        let f = Fake { openai: Ok(Some(10)), codex: None, calls: Cell::new(0) };
        let _ = self_compose(&f, dir.path(), "fp", t0()).await;
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(120)).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(10));
        assert!(!cp.openai_stale);
        assert_eq!(f.calls.get(), 1, "gated to one fetch per 300s");
    }

    #[tokio::test]
    async fn expired_cache_refetches() {
        let dir = tempdir().unwrap();
        let f = Fake { openai: Ok(Some(10)), codex: None, calls: Cell::new(0) };
        let _ = self_compose(&f, dir.path(), "fp", t0()).await;
        let _ = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(301)).await;
        assert_eq!(f.calls.get(), 2);
    }

    #[tokio::test]
    async fn fetch_error_serves_stale_value_marked() {
        let dir = tempdir().unwrap();
        cache::write_success(dir.path(), "fp", 999, t0());
        let f = Fake { openai: Err("boom".into()), codex: None, calls: Cell::new(0) };
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(400)).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(999));
        assert!(cp.openai_stale, "stale value marked");
        assert_eq!(f.calls.get(), 1);
    }

    #[tokio::test]
    async fn cooldown_skips_fetch() {
        let dir = tempdir().unwrap();
        cache::write_success(dir.path(), "fp", 999, t0());
        cache::write_failure(dir.path(), "fp", t0() + Duration::seconds(400));
        let f = Fake { openai: Ok(Some(123)), codex: None, calls: Cell::new(0) };
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(420)).await;
        assert_eq!(f.calls.get(), 0, "in cooldown, no fetch");
        assert_eq!(cp.openai_cost_micro_usd, Some(999));
        assert!(cp.openai_stale);
    }

    #[tokio::test]
    async fn no_key_yields_no_openai_cell() {
        let dir = tempdir().unwrap();
        let f = Fake { openai: Ok(None), codex: Some(3.0), calls: Cell::new(0) };
        let cp = self_compose(&f, dir.path(), "fp", t0()).await;
        assert_eq!(cp.openai_cost_micro_usd, None);
        assert!(!cp.openai_stale);
        assert_eq!(cp.codex_used_percent, Some(3.0));
    }
}
```

Note: `statusline_render` already depends on `tokio`? It does NOT (Task recon). The `#[tokio::test]` macro needs `tokio` as a **dev-dependency** with the `macros` + `rt` features. Add to `crates/statusline_render/Cargo.toml` under `[dev-dependencies]`:

```toml
tokio = { workspace = true, features = ["macros", "rt"] }
```

(`self_compose` itself is `async` but spawns nothing and needs no runtime feature in the main crate - only the test harness does.)

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo nextest run -p statusline_render self_compose`
Expected: FAIL - `cannot find ... CrossSources` / `self_compose`.

- [ ] **Step 3: Write the implementation** at the top of `crates/statusline_render/src/self_compose.rs`:

```rust
//! Self-compose path: build cross-provider segments without the watcher.
//!
//! The statusline calls this only when there is no fresh `snapshot.json`. It
//! reads Codex locally (cheap, every turn) and serves the OpenAI cost figure
//! through the cache (cache.rs) so the billing API is hit at most once per 300s
//! (AGENTS.md 3.1). It calls ONLY the two sources behind `CrossSources` - never
//! the Anthropic OAuth path (5.4 politeness invariant); that is why this crate
//! has no `anthropic_oauth` dependency.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::cache;
use crate::render::CrossProvider;

/// The two cross-provider sources, abstracted so the orchestrator (and its
/// once-per-300s gate) is testable without network. The real implementation
/// lives in `balanze_cli` (`LiveCrossSources`).
#[allow(async_fn_in_trait)]
pub trait CrossSources {
    /// `Ok(Some(v))` = fetched v (micro-USD); `Ok(None)` = no OpenAI key
    /// configured (no cell, no cooldown); `Err(msg)` = the fetch attempt failed
    /// (triggers the negative cooldown and keeps any prior value).
    async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String>;
    /// Local Codex quota percent (0..100), or `None` if Codex is absent/unparsed.
    fn codex_used_percent(&self) -> Option<f32>;
}

pub async fn self_compose<S: CrossSources>(
    sources: &S,
    cache_dir: &Path,
    fingerprint: &str,
    now: DateTime<Utc>,
) -> CrossProvider {
    // Codex: local, cheap, never cached -> current whenever present.
    let codex_used_percent = sources.codex_used_percent();

    // OpenAI: cache-gated.
    let entry = cache::read(cache_dir, fingerprint);
    let last_val = entry.as_ref().and_then(|e| e.total_micro_usd);
    let fresh = entry.as_ref().is_some_and(|e| cache::is_fresh(e, now));
    let cooled = entry.as_ref().is_some_and(|e| cache::in_cooldown(e, now));

    let (openai_cost_micro_usd, openai_stale) = if fresh {
        (last_val, false)
    } else if cooled {
        (last_val, last_val.is_some())
    } else {
        match sources.fetch_openai_total_micro_usd().await {
            Ok(Some(v)) => {
                cache::write_success(cache_dir, fingerprint, v, now);
                (Some(v), false)
            }
            Ok(None) => (None, false),
            Err(e) => {
                tracing::debug!("statusline: OpenAI self-compose fetch failed: {e}");
                cache::write_failure(cache_dir, fingerprint, now);
                (last_val, last_val.is_some())
            }
        }
    };

    CrossProvider {
        codex_used_percent,
        openai_cost_micro_usd,
        codex_stale: false,
        openai_stale,
    }
}
```

- [ ] **Step 4: Re-export** from `crates/statusline_render/src/lib.rs`: add `mod self_compose;` and extend the re-export line, e.g.:

```rust
pub mod cache;
mod render;
mod self_compose;
pub mod style;
pub use render::{CrossProvider, RenderInput, render};
pub use self_compose::{CrossSources, self_compose};
```

- [ ] **Step 5: Run to confirm pass.**

Run: `cargo nextest run -p statusline_render` (all green, incl. the 6 self_compose tests), then `cargo clippy -p statusline_render --all-targets -- -D warnings`.

- [ ] **Step 6: Commit.**

```bash
git add crates/statusline_render/src/self_compose.rs crates/statusline_render/src/lib.rs crates/statusline_render/Cargo.toml
git commit -m "feat(statusline): CrossSources trait + cache-gated self_compose orchestrator"
```

---

### Task 4: `LiveCrossSources` I/O adapter in `balanze_cli`

**Goal:** The real `CrossSources` implementation - Codex via `codex_local`, OpenAI via a short-timeout `costs_this_month` with the OAuth-free key resolution extracted for reuse - plus a `BALANZE_OPENAI_API_BASE` test seam. Never touches the OAuth path.

**Files:**
- Modify: `crates/balanze_cli/src/sources.rs`

**Acceptance Criteria:**
- [ ] `pub(crate) fn resolve_openai_key() -> anyhow::Result<Option<String>>` returns `Ok(Some)` for a non-empty trimmed `BALANZE_OPENAI_KEY`, `Ok(None)` for empty env or keychain `NotFound`, and propagates a non-`NotFound` keychain error as `Err`; `live_fetch_openai` is refactored to call it via `?` (DRY, behavior EXACTLY preserved including the propagated keychain error).
- [ ] `LiveCrossSources` implements `statusline_render::CrossSources`: `codex_used_percent` maps `codex_local::read_codex_quota()`'s `primary.used_percent` to `f32` (None on `Ok(None)`/`FileMissing`/error); `fetch_openai_total_micro_usd` returns `Ok(None)` when no key, `Ok(Some(total_micro_usd))` on success, `Err(_)` on fetch failure, using a 3s-timeout client + `BackoffPolicy::fail_fast()`.
- [ ] The OpenAI base URL is `BALANZE_OPENAI_API_BASE` when set, else the existing production base.
- [ ] No call path in `LiveCrossSources` reaches `live_fetch_oauth` / `fetch_oauth` / `anthropic_oauth`.
- [ ] `cargo build -p balanze_cli` succeeds; existing `sources`/`probes` behavior unchanged.

**Verify:** `cargo build -p balanze_cli && cargo nextest run -p balanze_cli` -> green

**Steps:**

- [ ] **Step 1: Extract `resolve_openai_key`.** In `crates/balanze_cli/src/sources.rs`, add (near the existing `live_fetch_openai`):

```rust
/// Resolve the OpenAI admin key: `BALANZE_OPENAI_KEY` (trimmed; empty -> None)
/// takes precedence, else the OS keychain entry. `Ok(None)` = not configured;
/// `Err` = a real keychain failure (not just "absent"). Single source of truth
/// for both the snapshot fetch and the statusline self-compose fingerprint.
pub(crate) fn resolve_openai_key() -> anyhow::Result<Option<String>> {
    if let Ok(env_key) = std::env::var("BALANZE_OPENAI_KEY") {
        let trimmed = env_key.trim();
        return Ok(if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        });
    }
    match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(k) => Ok(Some(k)),
        Err(keychain::KeychainError::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
```

Then refactor the top of `live_fetch_openai` to use it - replace its inline key block with:

```rust
    let key = match resolve_openai_key()? {
        Some(k) => k,
        None => return Ok(None),
    };
```

Keep the rest of `live_fetch_openai` (client build, `costs_this_month`, error mapping) exactly as-is. This preserves `live_fetch_openai`'s original behavior EXACTLY: empty env -> `Ok(None)`, keychain `NotFound` -> `Ok(None)`, and a non-`NotFound` keychain error propagates via `?` (as it did before). The statusline path (Task 5 + `LiveCrossSources` below) is the one that degrades any resolution failure to "no key" - it must never error a prompt.

- [ ] **Step 2: Add the base-URL seam.** Find the existing OpenAI base constant in `sources.rs` (e.g. `OPENAI_API_BASE`). Add a resolver:

```rust
/// Production OpenAI base, overridable via `BALANZE_OPENAI_API_BASE` (a test
/// seam; lets integration tests point the self-compose fetch at wiremock).
fn openai_api_base() -> String {
    std::env::var("BALANZE_OPENAI_API_BASE").unwrap_or_else(|_| OPENAI_API_BASE.to_string())
}
```

(If `live_fetch_openai` currently passes the const directly, leave it; only `LiveCrossSources` needs the override. If you prefer, route `live_fetch_openai` through `openai_api_base()` too - harmless and consistent.)

- [ ] **Step 3: Implement `LiveCrossSources`.** Add to `sources.rs`:

```rust
use std::time::Duration;

/// The real cross-provider sources for the statusline self-compose path.
/// Codex = local files; OpenAI = Admin Costs API behind a short timeout. Calls
/// NEITHER the Anthropic OAuth path NOR `snapshot_composer::compose` (5.4).
pub(crate) struct LiveCrossSources;

impl statusline_render::CrossSources for LiveCrossSources {
    async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String> {
        // A missing OR unreadable key -> no OpenAI cell (a statusline never errors).
        let key = match resolve_openai_key() {
            Ok(Some(k)) => k,
            _ => return Ok(None),
        };
        // Short timeout: the statusline runs every turn; never hang the prompt.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?;
        let costs = openai_client::costs_this_month(
            &client,
            &openai_api_base(),
            &key,
            &backoff::BackoffPolicy::fail_fast(),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(Some(costs.total_micro_usd))
    }

    fn codex_used_percent(&self) -> Option<f32> {
        match codex_local::read_codex_quota() {
            Ok(Some(q)) => Some(q.primary.used_percent as f32),
            _ => None,
        }
    }
}
```

Ensure `reqwest`, `openai_client`, `codex_local`, `backoff`, and `statusline_render` are dependencies of `balanze_cli` (they already are - `live_fetch_openai` uses the first three; `statusline_render` is used by `statusline.rs`). Add `reqwest`/`backoff` to the dep list only if the compiler reports them missing.

- [ ] **Step 4: Build + lint.**

Run: `cargo build -p balanze_cli` then `cargo clippy -p balanze_cli --all-targets -- -D warnings`
Expected: clean. (No new unit test here: the live OpenAI fetch is covered by `openai_client`'s wiremock suite + the Task 6 integration test; the trait orchestration is covered by Task 3.)

- [ ] **Step 5: Commit.**

```bash
git add crates/balanze_cli/src/sources.rs
git commit -m "feat(statusline): LiveCrossSources adapter (codex local + short-timeout OpenAI), OAuth-free"
```

---

### Task 5: Wire self-compose into `statusline_cross_provider()`

**Goal:** Restructure the cross-provider resolver to the locked precedence - fresh snapshot wins; else self-compose (one-shot runtime + key fingerprint); else a stale snapshot as a never-blank fallback; else Claude-only - keeping `cmd_statusline` synchronous.

**Files:**
- Modify: `crates/balanze_cli/src/statusline.rs`

**Acceptance Criteria:**
- [ ] A fresh `snapshot.json` (age <= `SNAPSHOT_FRESHNESS_SECS`) is used directly with no runtime built and no network.
- [ ] When the snapshot is absent or stale, self-compose runs via a one-shot `tokio::runtime::Runtime` and `LiveCrossSources`, keyed by `cache::key_fingerprint(resolve_openai_key().as_deref())` and `cache::cache_dir_path()`.
- [ ] The selection tail is a pure `pick_cross(composed, stale_snapshot)`: composed-with-cells wins; else the stale snapshot (never-blank); else `None`. Unit-tested for all four combinations.
- [ ] If `cache_dir_path()` is `None` or the runtime fails to build, `self_compose_cross` returns `None` and the path falls back to the stale snapshot or `None` - never panics.
- [ ] Claude segments are unaffected; existing statusline tests stay green.

**Verify:** `cargo nextest run -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings`

**Steps:**

- [ ] **Step 1: Rewrite `statusline_cross_provider()`** in `crates/balanze_cli/src/statusline.rs`. Replace the whole function with these three functions:

```rust
/// Resolve cross-provider data (Codex %, OpenAI $) for the statusline.
///
/// Precedence (see PR3 plan): a fresh host-written `snapshot.json` wins (zero
/// network); otherwise self-compose Codex + OpenAI directly (5.4: never via the
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
/// cannot be built (never panics).
fn self_compose_cross(
    now: chrono::DateTime<chrono::Utc>,
) -> Option<statusline_render::CrossProvider> {
    let cache_dir = statusline_render::cache::cache_dir_path()?;
    let fingerprint = statusline_render::cache::key_fingerprint(
        crate::sources::resolve_openai_key().ok().flatten().as_deref(),
    );
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
```

Note the threaded `now`: `cross_from_payload` already takes a `now` argument, so reuse the single `now` for both the snapshot age check and self-compose (one clock read per render). Keep `cross_from_payload` and `SNAPSHOT_FRESHNESS_SECS` as they are (Task 2 updated `cross_from_payload`'s fields).

- [ ] **Step 2: Verify imports.** Ensure `crate::sources::{resolve_openai_key, LiveCrossSources}` are reachable (they are `pub(crate)` from Task 4). `tokio` is already a `balanze_cli` dependency (used by `watch_cmd`, `export`).

- [ ] **Step 3: Add `pick_cross` unit tests.** In the `#[cfg(test)] mod tests` of `statusline.rs`, add (using a small builder for clarity):

```rust
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
        let got = pick_cross(Some(composed), Some(snap)).unwrap();
        assert_eq!(got.codex_used_percent, Some(5.0));
        assert_eq!(got.openai_cost_micro_usd, None);
    }

    #[test]
    fn pick_cross_falls_back_to_stale_snapshot_when_composed_empty() {
        let composed = cp(None, None);
        let snap = cp(None, Some(99));
        let got = pick_cross(Some(composed), Some(snap)).unwrap();
        assert_eq!(got.openai_cost_micro_usd, Some(99));
    }

    #[test]
    fn pick_cross_falls_back_when_composed_absent() {
        let snap = cp(Some(1.0), None);
        let got = pick_cross(None, Some(snap)).unwrap();
        assert_eq!(got.codex_used_percent, Some(1.0));
    }

    #[test]
    fn pick_cross_none_when_nothing_available() {
        assert!(pick_cross(Some(cp(None, None)), None).is_none());
        assert!(pick_cross(None, None).is_none());
    }
```

- [ ] **Step 4: Confirm the existing absent-snapshot test still holds.** The PR2 test `cross_provider_returns_none_when_snapshot_absent` asserted `None` when the snapshot is absent. With self-compose, an absent snapshot now triggers self-compose. In CI (no Codex files, no OpenAI key) self-compose yields no cells -> `pick_cross` returns `None`, so the test should still pass. **Verify this**; if the environment might have real Codex files or a key, make it deterministic via the existing `EnvGuard`/`ENV_LOCK` helpers - set `CODEX_CONFIG_DIR` + `BALANZE_CACHE_DIR_OVERRIDE` to empty temp dirs and `BALANZE_OPENAI_KEY=""` - and rename it to `cross_provider_none_when_snapshot_absent_and_no_self_compose_data`.

- [ ] **Step 5: Run + lint.**

Run: `cargo nextest run -p balanze_cli` (green) and `cargo clippy -p balanze_cli --all-targets -- -D warnings` (clean).

- [ ] **Step 6: Commit.**

```bash
git add crates/balanze_cli/src/statusline.rs
git commit -m "feat(statusline): self-compose fallback when no fresh snapshot (fresh>self-compose>stale)"
```

---

### Task 6: Integration test + documentation

**Goal:** An end-to-end test proving self-compose renders a cross-provider line with no snapshot and gates the OpenAI fetch to once per 300s, plus the doc updates the change requires.

**Files:**
- Create: `crates/balanze_cli/tests/integration_statusline_self_compose.rs`
- Modify: `docs/ARCHITECTURE.md`, `AGENTS.md`, `docs/TROUBLESHOOTING.md`

**Acceptance Criteria:**
- [ ] An integration test, with `BALANZE_DATA_DIR_OVERRIDE` pointing at an empty dir (no snapshot), `BALANZE_CACHE_DIR_OVERRIDE` at a temp dir, `BALANZE_OPENAI_API_BASE` at a wiremock server returning a costs payload, and `BALANZE_OPENAI_KEY` set, renders a line containing `OpenAI $` via self-compose.
- [ ] The wiremock mock is `.expect(1)` and two consecutive renders within the TTL keep it at one request (the §3.1 gate), proven by the mock's verification on drop.
- [ ] `docs/ARCHITECTURE.md` documents the self-compose path, the `<cache>/statusline/openai-cost.json` artifact, `BALANZE_CACHE_DIR_OVERRIDE`, and that the statusline now honors the §3.1 OpenAI gate via the cache.
- [ ] `AGENTS.md` §3.1 notes the statusline self-compose path obeys the 5-min OpenAI gate through the 300s cache (and never calls Anthropic OAuth).
- [ ] `docs/TROUBLESHOOTING.md` gains an entry: cross-provider segments appear even without the desktop app/watcher running, refreshing OpenAI at most once per 5 min.
- [ ] No em-dash or Unicode ellipsis introduced (§3.5).

**Verify:** `cargo nextest run -p balanze_cli --test integration_statusline_self_compose` -> green; `grep -nP "\x{2014}|\x{2026}"` over the changed docs -> no matches.

**Steps:**

- [ ] **Step 1: Use the subprocess harness (deterministic).** Do NOT call the statusline path in-process: `statusline_cross_provider` builds its own `tokio::runtime::Runtime`, which panics if called from inside a `#[tokio::test]`. Invoke the built `balanze-cli` binary as a subprocess via `assert_cmd` instead - it gets its own process + runtime, while wiremock serves HTTP from the test process. This also exercises the real `cmd_statusline` end to end. Confirm `assert_cmd` and `wiremock` are in `crates/balanze_cli/Cargo.toml` `[dev-dependencies]`; add them if missing (`assert_cmd = "2"`, `wiremock = { workspace = true }` - `openai_client` already uses `wiremock`). `serde_json` and `tempfile` are already dev-deps.

- [ ] **Step 2: Write the integration test.** Create `crates/balanze_cli/tests/integration_statusline_self_compose.rs`:

```rust
//! Self-compose end-to-end: no snapshot present, OpenAI composed directly and
//! gated to one fetch per 300s. Drives the real `balanze-cli statusline` binary
//! against a wiremock Admin Costs API.

use assert_cmd::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal valid `/v1/organization/costs` body. Align with the shape
/// `openai_client` parses (check `crates/openai_client/tests/`); value 4.20 USD
/// -> total_micro_usd 4_200_000 -> rendered "OpenAI $4.20".
fn costs_body() -> serde_json::Value {
    serde_json::json!({
        "object": "page",
        "data": [{
            "object": "bucket",
            "start_time": 0,
            "end_time": 1,
            "results": [{
                "object": "organization.costs.result",
                "amount": { "value": 4.20, "currency": "usd" },
                "line_item": "gpt-5"
            }]
        }],
        "has_more": false,
        "next_page": null
    })
}

#[tokio::test]
async fn self_compose_renders_openai_and_gates_to_one_fetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(costs_body()))
        .expect(1) // the 300s cache must collapse two renders into one fetch
        .mount(&server)
        .await;

    let data_dir = tempfile::tempdir().unwrap(); // empty -> no snapshot.json -> self-compose
    let cache_dir = tempfile::tempdir().unwrap();
    let codex_dir = tempfile::tempdir().unwrap(); // empty -> Codex absent, focus on OpenAI
    let base = server.uri();

    // Two renders within the TTL: the cache must yield exactly one upstream GET.
    for _ in 0..2 {
        let (data, cache, codex, base) = (
            data_dir.path().to_path_buf(),
            cache_dir.path().to_path_buf(),
            codex_dir.path().to_path_buf(),
            base.clone(),
        );
        let out = tokio::task::spawn_blocking(move || {
            Command::cargo_bin("balanze-cli")
                .unwrap()
                .arg("statusline")
                .env("BALANZE_DATA_DIR_OVERRIDE", &data)
                .env("BALANZE_CACHE_DIR_OVERRIDE", &cache)
                .env("BALANZE_OPENAI_API_BASE", &base)
                .env("BALANZE_OPENAI_KEY", "sk-test")
                .env("CODEX_CONFIG_DIR", &codex)
                .env("NO_COLOR", "1")
                .write_stdin(r#"{"version":"2.1.144","model":{"display_name":"Sonnet"}}"#)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("OpenAI $"), "self-composed OpenAI segment missing; got: {stdout}");
    }
    // `server` drops here; `.expect(1)` is verified on drop -> two renders, one fetch.
}
```

Notes for the implementer: `Command::cargo_bin("balanze-cli")` resolves the workspace binary (bin name is `balanze-cli`). The binary calls `keychain::init_default_store()` at startup, but with `BALANZE_OPENAI_KEY` set, `resolve_openai_key` never touches the keychain - so no OS-keychain dependency in CI. If `openai_client`'s parser rejects the inline body shape, copy the exact body from its wiremock test fixture.

- [ ] **Step 3: Run the integration test.**

Run: `cargo nextest run -p balanze_cli --test integration_statusline_self_compose`
Expected: PASS, and wiremock's `.expect(1)` confirms two renders -> one fetch.

- [ ] **Step 4: Update `docs/ARCHITECTURE.md`.** In the IPC / on-disk files section that already lists `snapshot.json` and `statusline.snapshot.json`, add the cache artifact and self-compose path. Add a row/paragraph like:

> `<ProjectDirs.cache>/statusline/openai-cost.json` - written and read by `balanze-cli statusline` on the self-compose fallback path (no watcher/desktop running). One global, key-fingerprinted entry; 300s TTL = the AGENTS.md §3.1 OpenAI politeness gate; negative-failure cooldown; stale-while-updating. Overridable via `BALANZE_CACHE_DIR_OVERRIDE`. Self-compose reads Codex locally and OpenAI through this cache; it never calls Anthropic OAuth (§5.4).

Also confirm `statusline_render` in the crate map mentions it now owns the cache + self-compose orchestration (the `CrossSources` trait, real impl `LiveCrossSources` in `balanze_cli`).

- [ ] **Step 5: Update `AGENTS.md` §3.1.** Append to the OpenAI/HTTP discipline bullets a note that the statusline self-compose path obeys the same 5-minute OpenAI gate via a 300s on-disk cache (key-fingerprinted, negative cooldown) and uses a short (3s) client timeout, and that it never calls the Anthropic OAuth usage endpoint. Keep it one or two sentences; no em-dash/ellipsis.

- [ ] **Step 6: Update `docs/TROUBLESHOOTING.md`.** Add an entry: "Cross-provider segments (Codex %, OpenAI $) appear in the statusline even when the desktop app / watcher is not running - the statusline self-composes them, refreshing OpenAI at most once per 5 minutes and showing a `⚠` marker when serving a cached value after a failed refresh."

- [ ] **Step 7: Em-dash / ellipsis sweep + full validation.**

Run:
```bash
git diff --cached --name-only | grep -E '\.(md|rs)$' | xargs grep -nP "\x{2014}|\x{2026}" || echo "clean"
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: "clean" + all green.

- [ ] **Step 8: Commit.**

```bash
git add crates/balanze_cli/tests/integration_statusline_self_compose.rs docs/ARCHITECTURE.md AGENTS.md docs/TROUBLESHOOTING.md
git commit -m "test(statusline): self-compose integration + cross-provider gate proof; docs"
```

---

## Manual QA at the PR3 boundary (for the human, after merge-readiness)

1. Stop any running `balanze-cli watch` / desktop app so no fresh `snapshot.json` exists, then look at the live Claude Code statusline: it should still show `◇Codex N%` and `OpenAI $X` (self-composed). Cross-check both against reality (Codex usage; OpenAI Admin Costs dashboard) - the real-data cross-reference.
2. Confirm politeness: with `BALANZE_LOG=debug`, the OpenAI fetch should fire at most once per ~5 min across many turns (the cache gate). It must NOT trigger any Anthropic OAuth call / 429 during an active Claude session.
3. Rotate the OpenAI key (or change `BALANZE_OPENAI_KEY`) and confirm the next render refetches (fingerprint invalidation).
4. Temporarily break the OpenAI key and confirm the line shows the last value with `⚠` rather than blanking, and recovers after cooldown when the key is restored.
5. Confirm a running watcher still wins (fresh snapshot path) and no second fetch happens while it is up.

## Out of scope (deferred, tracked elsewhere)

- Codex micro-cache (spec §5.3 "optional"; local read is cheap - YAGNI here).
- `claude_statusline` transcript-path parsing (the global fingerprint-keyed cache makes it unnecessary).
- PR4 (replace-any-statusline flow) and PR5 (Codex preset + docs + release).
