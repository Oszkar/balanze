# Track B — De-risk Before Any Poller: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the snapshot-composition policy into a shared, fixture-testable `snapshot_composer` crate (CLI ≡ future watcher, no behavior change), and add a budget-parameterized exponential-backoff layer to the HTTP clients so a poller can exist without tight-looping on 429s — while the one-shot CLI stays snappy via a fail-fast budget.

**Architecture:** Two independent sub-systems. (A) `crates/snapshot_composer`: a `SnapshotSources` trait at the I/O boundary + a pure `compose()` policy fn holding the exact orchestration moved verbatim from `balanze_cli::build_snapshot`; `balanze_cli` provides `LiveSources`, the integration test provides `FixtureSources` so the *same* `compose()` the CLI runs is exercised end-to-end without network/fs. (B) `crates/backoff`: a pure `BackoffPolicy` schedule + a generic `retry` combinator; wired inside `anthropic_oauth` + `openai_client` with the policy as a parameter — the CLI passes `fail_fast()` (0 retries), the future watcher will pass `standard()` (30s×2ⁿ, cap 10 min).

**Tech Stack:** Rust 2021 / MSRV 1.77, `tokio` (async + `time` for sleep, `test-util` for paused-time tests), `anyhow`/`thiserror`, `chrono`, `tracing`. No third-party crates added (native `async fn` in traits is 1.75+; static dispatch only — no `async-trait`).

**Decisions already made (do not re-litigate):** composer is a NEW crate `snapshot_composer` (not a `state_coordinator` fn — that crate must stay a fetch-free actor receiving partials). Backoff is built now as a tested pure unit + combinator and wired into the clients with a budget knob; the one-shot CLI passes `fail_fast()` so `balanze-cli status` never blocks minutes on a 429. Retryable set: 429 + transport/timeout + 5xx; **never 401** (it stays on Track A's existing refresh→retry path). `Retry-After` honored when present, clamped to the policy cap. No jitter (single-user, single client — no thundering herd). Step A is strictly behavior-preserving; Step B intentionally adds retry behavior (with fail-fast wired for the CLI, so observable CLI behavior is unchanged in the common no-429 case).

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/snapshot_composer/Cargo.toml` | New crate manifest (deps: the backend crates, NOT reqwest) | Create |
| `crates/snapshot_composer/src/lib.rs` | `SnapshotSources` trait + `compose()` policy + private pure `compute_anthropic_api_cost`; in-crate fake-sources unit tests of the divergence-critical rules | Create |
| `crates/balanze_cli/src/main.rs` | Replace `build_snapshot` + helpers with `LiveSources: SnapshotSources` + `compose(&LiveSources, now)`; delete the now-moved helpers; rewrite the `TODO(v0.2)` comment | Modify |
| `crates/balanze_cli/Cargo.toml` | Add `snapshot_composer` path dep | Modify |
| `crates/balanze_cli/tests/integration_4quadrant.rs` | Add `FixtureSources: SnapshotSources` + a test asserting `compose(&FixtureSources, fixed_now)` — the real parity guard | Modify |
| `crates/backoff/Cargo.toml` | New crate manifest (tokio time; dev tokio test-util) | Create |
| `crates/backoff/src/lib.rs` | `BackoffPolicy` (`standard`/`fail_fast`/`custom`) + `RetryDecision` + generic `retry` combinator; paused-time unit tests | Create |
| `crates/anthropic_oauth/src/types.rs` | Add `OAuthError::RateLimited { retry_after: Option<Duration> }` | Modify |
| `crates/anthropic_oauth/src/client.rs` | `fetch_usage` gains `policy: &BackoffPolicy`; wrap the HTTP attempt in `backoff::retry` with an oauth classifier; capture 429 `Retry-After` | Modify |
| `crates/anthropic_oauth/src/refresh.rs` | `refresh_access_token` gains `policy: &BackoffPolicy`; same retry wrap | Modify |
| `crates/anthropic_oauth/Cargo.toml` | Add `backoff` path dep | Modify |
| `crates/anthropic_oauth/tests/refresh_wiremock.rs` | Update call sites; add a 429-then-200 retry test + a 401-does-not-retry test | Modify |
| `crates/openai_client/src/types.rs` | Add `OpenAiError::RateLimited { retry_after: Option<Duration> }` | Modify |
| `crates/openai_client/src/client.rs` | `fetch_costs`/`costs_this_month` gain `policy: &BackoffPolicy`; retry wrap + `Retry-After` | Modify |
| `crates/openai_client/Cargo.toml` | Add `backoff` path dep | Modify |
| `crates/openai_client/tests/wiremock_tests.rs` | Update call sites; add 429-then-200 + 403-does-not-retry tests | Modify |
| `AGENTS.md` | Repo Map: add `snapshot_composer` + `backoff` lines; §2 YAGNI allowlist note; §4 boundary #8 mark extracted | Modify |
| `docs/prd.md` | One-line note under Phase 2 Track B that it shipped | Modify |

`crates/*` are auto-workspace-members (`members = ["src-tauri", "crates/*"]`) so no root `Cargo.toml` edit is needed.

---

### Task 1: `snapshot_composer` crate — trait + policy + parity unit tests

**Goal:** A new crate exposing `SnapshotSources` (I/O boundary) and `compose()` containing the exact orchestration policy currently in `balanze_cli::build_snapshot`, unit-tested with in-crate fake sources covering every divergence-critical rule.

**Files:**
- Create: `crates/snapshot_composer/Cargo.toml`
- Create: `crates/snapshot_composer/src/lib.rs`

**Acceptance Criteria:**
- [ ] `pub trait SnapshotSources` with four `async fn`: `fetch_oauth`, `load_claude_events`, `fetch_codex_quota`, `fetch_openai` (signatures below).
- [ ] `pub async fn compose<S: SnapshotSources>(sources: &S, now: DateTime<Utc>) -> Snapshot` reproduces `build_snapshot`'s policy byte-for-byte: oauth→`five_hour_reset` window anchor; JSONL Ok ⇒ window + cost, JSONL Err ⇒ only `claude_jsonl_error` (both Anthropic cells stay None, no duplicate error); Codex `Ok(None)`/OpenAI `Ok(None)` set no error; direct `Snapshot{..}` construction.
- [ ] In-crate tests with a configurable fake `SnapshotSources` assert: (a) all-ok snapshot fully populated + window anchored to the oauth reset; (b) oauth Err ⇒ `claude_oauth_error` set, anchor falls back (window_start == now − 5h); (c) JSONL Err ⇒ `claude_jsonl_error` set AND `anthropic_api_cost`/`anthropic_api_cost_error`/`claude_jsonl` all None; (d) Codex `Ok(None)` ⇒ `codex_quota`+`codex_quota_error` both None; (e) OpenAI `Ok(None)` ⇒ `openai`+`openai_error` both None.
- [ ] `cargo clippy -p snapshot_composer --all-targets -- -D warnings` clean (the `#[allow(async_fn_in_trait)]` carries the documented rationale).
- [ ] Crate does NOT depend on `reqwest`.

**Verify:** `cargo test -p snapshot_composer && cargo clippy -p snapshot_composer --all-targets -- -D warnings`

**Steps:**

- [ ] **Step 1: Create `crates/snapshot_composer/Cargo.toml`**

```toml
[package]
name = "snapshot_composer"
description = "Shared source-orchestration policy: composes the backend crates into one Snapshot. The single composition path used by balanze_cli today and the watcher/pollers later (AGENTS.md §4 #8) so they cannot silently diverge."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
publish.workspace = true

[dependencies]
anthropic_oauth = { path = "../anthropic_oauth" }
claude_cost = { path = "../claude_cost" }
claude_parser = { path = "../claude_parser" }
codex_local = { path = "../codex_local" }
openai_client = { path = "../openai_client" }
state_coordinator = { path = "../state_coordinator" }
window = { path = "../window" }
anyhow = { workspace = true }
chrono = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

- [ ] **Step 2: Write the failing tests first**

Create `crates/snapshot_composer/src/lib.rs` with ONLY the test module + minimal type stubs so it compiles-then-fails. Put this at the bottom of the file (the impl above it comes in Step 3; write the tests now, expect a compile failure until Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap()
    }

    // A fake source set whose four results are individually configurable.
    #[derive(Default)]
    struct Fake {
        oauth: Option<anyhow::Result<ClaudeOAuthSnapshot>>,
        events: Option<anyhow::Result<(Vec<UsageEvent>, usize)>>,
        codex: Option<anyhow::Result<Option<CodexQuotaSnapshot>>>,
        openai: Option<anyhow::Result<Option<OpenAiCosts>>>,
    }
    impl SnapshotSources for Fake {
        async fn fetch_oauth(&self) -> anyhow::Result<ClaudeOAuthSnapshot> {
            match &self.oauth {
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Err(anyhow::anyhow!("oauth not configured in fake")),
            }
        }
        async fn load_claude_events(&self) -> anyhow::Result<(Vec<UsageEvent>, usize)> {
            match &self.events {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok((Vec::new(), 0)),
            }
        }
        async fn fetch_codex_quota(&self) -> anyhow::Result<Option<CodexQuotaSnapshot>> {
            match &self.codex {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok(None),
            }
        }
        async fn fetch_openai(&self) -> anyhow::Result<Option<OpenAiCosts>> {
            match &self.openai {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok(None),
            }
        }
    }

    fn one_event(now: DateTime<Utc>) -> UsageEvent {
        use claude_parser::{AccountType, DataSource, Provider};
        UsageEvent {
            ts: now - chrono::Duration::minutes(10),
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: None,
            request_id: None,
        }
    }

    #[tokio::test]
    async fn jsonl_error_keeps_both_anthropic_cells_none_with_single_error() {
        let f = Fake {
            events: Some(Err(anyhow::anyhow!("permission denied"))),
            ..Default::default()
        };
        let snap = compose(&f, now()).await;
        assert!(snap.claude_jsonl.is_none());
        assert_eq!(snap.claude_jsonl_error.as_deref(), Some("permission denied"));
        assert!(snap.anthropic_api_cost.is_none());
        assert!(
            snap.anthropic_api_cost_error.is_none(),
            "the JSONL error must NOT be duplicated into the cost cell"
        );
    }

    #[tokio::test]
    async fn codex_and_openai_none_set_no_error() {
        let f = Fake {
            events: Some(Ok((vec![one_event(now())], 1))),
            ..Default::default()
        };
        let snap = compose(&f, now()).await;
        assert!(snap.codex_quota.is_none() && snap.codex_quota_error.is_none());
        assert!(snap.openai.is_none() && snap.openai_error.is_none());
        assert!(snap.claude_jsonl.is_some(), "events Ok ⇒ jsonl populated");
        assert!(snap.anthropic_api_cost.is_some(), "events Ok ⇒ cost populated");
    }

    #[tokio::test]
    async fn oauth_error_falls_back_to_now_relative_window() {
        let f = Fake {
            oauth: Some(Err(anyhow::anyhow!("AuthExpired"))),
            events: Some(Ok((vec![one_event(now())], 1))),
            ..Default::default()
        };
        let snap = compose(&f, now()).await;
        assert_eq!(snap.claude_oauth_error.as_deref(), Some("AuthExpired"));
        let w = snap.claude_jsonl.unwrap().window;
        assert_eq!(
            w.window_start,
            now() - window::DEFAULT_WINDOW,
            "no oauth ⇒ no anchor ⇒ now-relative window"
        );
    }
}
```

- [ ] **Step 3: Implement the trait + `compose`**

Prepend to `crates/snapshot_composer/src/lib.rs` (above the test module):

```rust
//! Shared source-orchestration policy. This is the SINGLE composition path
//! (AGENTS.md §4 #8): `balanze_cli` runs it via `LiveSources`, the future
//! watcher will run it via its own `SnapshotSources` impl, and the
//! integration test runs it via `FixtureSources` — so the policy cannot
//! silently diverge between entry-points. Pure orchestration: it does no
//! network/filesystem I/O itself (that is the `SnapshotSources` impl's job)
//! and never imports `reqwest`.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, Utc};
use claude_parser::UsageEvent;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use state_coordinator::{JsonlSnapshot, Snapshot};
use tracing::{info, warn};
use window::{summarize_window, DEFAULT_BURN_WINDOW, DEFAULT_MIN_BURN_EVENTS, DEFAULT_WINDOW};

/// The four I/O-bound source fetches `compose` needs. CLI (`LiveSources`),
/// the future watcher, and tests (`FixtureSources`) provide impls. The trait
/// sits at the I/O boundary; the pure transforms (cost synthesis, window
/// math) live in `compose` so the orchestration policy is testable without
/// network/filesystem and is identical across entry-points.
///
/// `async fn` in a trait is stable since Rust 1.75 (MSRV here is 1.77). We
/// only ever use STATIC dispatch (`compose<S: SnapshotSources>`), never
/// `dyn SnapshotSources`, so the `async_fn_in_trait` lint's Send-bound
/// caveat does not apply — hence the documented allow.
#[allow(async_fn_in_trait)]
pub trait SnapshotSources {
    /// Anthropic OAuth usage. The impl owns credential load + proactive
    /// refresh + 401-retry (OAuth-fetch detail, not composition policy).
    async fn fetch_oauth(&self) -> anyhow::Result<ClaudeOAuthSnapshot>;
    /// All deduped Claude Code JSONL events + count of files scanned.
    async fn load_claude_events(&self) -> anyhow::Result<(Vec<UsageEvent>, usize)>;
    /// Codex rate-limit snapshot. `Ok(None)` = Codex not installed (NOT an error).
    async fn fetch_codex_quota(&self) -> anyhow::Result<Option<CodexQuotaSnapshot>>;
    /// OpenAI Admin Costs. `Ok(None)` = no key configured (NOT an error).
    async fn fetch_openai(&self) -> anyhow::Result<Option<OpenAiCosts>>;
}

/// Compose one `Snapshot` from the four sources, applying the exact
/// per-source error-mapping policy (AGENTS.md §4 #8). Moved verbatim from
/// the former `balanze_cli::build_snapshot`; behavior is unchanged.
pub async fn compose<S: SnapshotSources>(sources: &S, now: DateTime<Utc>) -> Snapshot {
    let (claude_oauth, claude_oauth_error) = match sources.fetch_oauth().await {
        Ok(s) => (Some(s), None),
        Err(e) => {
            warn!("OAuth source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    // Anchor the JSONL rolling window to Anthropic's authoritative 5-hour
    // reset when we have it (removes local clock-drift error); fall back to
    // now-relative when OAuth is unavailable. AGENTS.md v0.1.1 / §7.
    let window_anchor = claude_oauth
        .as_ref()
        .and_then(ClaudeOAuthSnapshot::five_hour_reset);

    // JSONL events power BOTH the window summary and the API-rate cost
    // synthesis. Read once, summarize twice. If the load fails entirely,
    // both downstream slots stay None and only claude_jsonl_error carries
    // the reason — we don't duplicate it into anthropic_api_cost_error.
    let mut claude_jsonl: Option<JsonlSnapshot> = None;
    let mut claude_jsonl_error: Option<String> = None;
    let mut anthropic_api_cost: Option<claude_cost::Cost> = None;
    let mut anthropic_api_cost_error: Option<String> = None;
    match sources.load_claude_events().await {
        Ok((events, files_scanned)) => {
            let window = summarize_window(
                &events,
                now,
                DEFAULT_WINDOW,
                DEFAULT_BURN_WINDOW,
                DEFAULT_MIN_BURN_EVENTS,
                window_anchor,
            );
            claude_jsonl = Some(JsonlSnapshot {
                files_scanned,
                window,
            });
            match compute_anthropic_api_cost(&events) {
                Ok(cost) => {
                    info!(
                        "claude_cost: total_micro_usd={} per_model_rows={} skipped={}",
                        cost.total_micro_usd,
                        cost.per_model.len(),
                        cost.skipped_models.len()
                    );
                    anthropic_api_cost = Some(cost);
                }
                Err(e) => {
                    warn!("anthropic_api_cost source failed: {e}");
                    anthropic_api_cost_error = Some(e.to_string());
                }
            }
        }
        Err(e) => {
            warn!("JSONL source failed: {e}");
            claude_jsonl_error = Some(e.to_string());
        }
    }

    let (codex_quota, codex_quota_error) = match sources.fetch_codex_quota().await {
        Ok(snap) => (snap, None),
        Err(e) => {
            warn!("codex_quota source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    let (openai, openai_error) = match sources.fetch_openai().await {
        Ok(Some(g)) => (Some(g), None),
        Ok(None) => (None, None),
        Err(e) => {
            warn!("OpenAI source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    Snapshot {
        fetched_at: now,
        claude_oauth,
        claude_oauth_error,
        claude_jsonl,
        claude_jsonl_error,
        anthropic_api_cost,
        anthropic_api_cost_error,
        codex_quota,
        codex_quota_error,
        openai,
        openai_error,
    }
}

/// Synthesize the API-rate cost from the JSONL events. Pure (no I/O);
/// moved verbatim from `balanze_cli::compute_anthropic_api_cost`.
fn compute_anthropic_api_cost(events: &[UsageEvent]) -> anyhow::Result<claude_cost::Cost> {
    let prices = claude_cost::load_bundled_prices()
        .map_err(|e| anyhow::anyhow!("claude_cost: bundled price table failed to load: {e}"))?;
    Ok(claude_cost::compute_cost(events, &prices))
}
```

- [ ] **Step 4: Run — expect green**

Run: `cargo test -p snapshot_composer`
Expected: the 3 tests pass. (`UsageEvent` field set is taken from `crates/window/src/lib.rs` test helpers; if a field name differs, read `crates/claude_parser/src/types.rs` and adjust the `one_event` literal — do NOT change the struct.)

- [ ] **Step 5: Gate + commit**

Run: `cargo fmt --all` then `cargo clippy -p snapshot_composer --all-targets -- -D warnings`

```bash
git add crates/snapshot_composer/
git commit -m "feat(snapshot_composer): shared compose() policy + SnapshotSources trait"
```

---

### Task 2: Repoint `balanze_cli` onto the composer (no behavior change)

**Goal:** `balanze_cli` implements `LiveSources: SnapshotSources` (the existing fetch bodies) and its `build_snapshot` becomes `compose(&LiveSources, Utc::now())`. The duplicated helpers are deleted. Observable CLI behavior is unchanged.

**Files:**
- Modify: `crates/balanze_cli/Cargo.toml` (add `snapshot_composer` dep)
- Modify: `crates/balanze_cli/src/main.rs` (replace `build_snapshot` + 549–651; relocate `fetch_oauth`/`load_and_dedup_claude_events`/`fetch_codex_quota`/`fetch_openai`/`refresh_and_persist`/`token_needs_refresh`/`REFRESH_MARGIN` into a `LiveSources` impl; delete `summarize_for_jsonl_snapshot` + `compute_anthropic_api_cost` — now in the composer)

**Acceptance Criteria:**
- [ ] `build_snapshot()` body is exactly `snapshot_composer::compose(&LiveSources, Utc::now()).await`.
- [ ] `LiveSources` implements all four trait methods; the bodies are the *current* helper bodies moved unchanged (oauth incl. proactive-refresh/401-retry; events = the load+dedup; codex = the `FileMissing ⇒ Ok(None)` mapping; openai = the env/keychain key resolution + `Ok(None)` when unconfigured).
- [ ] `summarize_for_jsonl_snapshot` and `compute_anthropic_api_cost` are removed from `main.rs` (the composer owns them); `window`/`claude_cost` direct deps in `balanze_cli` may remain only if still used elsewhere — otherwise leave the `Cargo.toml` deps (no behavior impact; do not churn).
- [ ] The `// TODO(v0.2):` block at 549–563 is replaced with a short note that the policy now lives in `snapshot_composer::compose` and the watcher will supply its own `SnapshotSources`.
- [ ] `cargo test --workspace` green; `cargo build -p balanze_cli` green; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all -- --check` clean.
- [ ] Behavior unchanged: `balanze-cli status` / `--json` / `--sections` produce the same output shape as before (the integration suite + existing unit tests guard this; do not modify them in this task).

**Verify:** `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`

**Steps:**

- [ ] **Step 1: Add the dep.** In `crates/balanze_cli/Cargo.toml`, under `[dependencies]` (alphabetical with the other path deps):

```toml
snapshot_composer = { path = "../snapshot_composer" }
```

- [ ] **Step 2: Replace `build_snapshot` + the TODO block.** In `crates/balanze_cli/src/main.rs`, replace lines 549–651 (the `// TODO(v0.2):` comment block through the end of `async fn build_snapshot`) with:

```rust
// The source-orchestration policy now lives in `snapshot_composer::compose`
// (AGENTS.md §4 #8): the CLI runs it via `LiveSources`, the future watcher
// will run it via its own `SnapshotSources` impl, and `integration_4quadrant`
// runs it via `FixtureSources` — one policy, no silent divergence.
async fn build_snapshot() -> Snapshot {
    snapshot_composer::compose(&LiveSources, Utc::now()).await
}

/// The production `SnapshotSources`: real network + filesystem + keychain.
/// Every method body below is the pre-extraction helper, moved unchanged.
struct LiveSources;

impl snapshot_composer::SnapshotSources for LiveSources {
    async fn fetch_oauth(&self) -> Result<ClaudeOAuthSnapshot> {
        live_fetch_oauth().await
    }
    async fn load_claude_events(&self) -> Result<(Vec<UsageEvent>, usize)> {
        load_and_dedup_claude_events()
    }
    async fn fetch_codex_quota(&self) -> Result<Option<codex_local::CodexQuotaSnapshot>> {
        live_fetch_codex_quota()
    }
    async fn fetch_openai(&self) -> Result<Option<OpenAiCosts>> {
        live_fetch_openai().await
    }
}
```

- [ ] **Step 3: Relocate the helper bodies unchanged.** Keep `load_and_dedup_claude_events` (currently 660–697) exactly as-is. Rename the three remaining helpers so the trait impl above resolves, moving their bodies UNCHANGED:
  - `fn fetch_codex_quota` (728–744) → rename to `fn live_fetch_codex_quota` (body identical).
  - `async fn fetch_oauth` (789–838) → rename to `async fn live_fetch_oauth` (body identical, including the `REFRESH_MARGIN`/`token_needs_refresh`/`refresh_and_persist` it calls — keep those three items 747–787 unchanged and in place).
  - `async fn fetch_openai` (850–884) → rename to `async fn live_fetch_openai` (body identical).
  - Delete `fn summarize_for_jsonl_snapshot` (699–717) and `fn compute_anthropic_api_cost` (719–723) entirely — the composer now owns them. Remove the now-dead `use window::{...}` import line (36) and the `JsonlSnapshot` import in line 34 only if nothing else in `main.rs` references them (grep first: `rg 'summarize_window|JsonlSnapshot|DEFAULT_WINDOW' crates/balanze_cli/src/main.rs`; remove only genuinely unused imports — clippy `-D warnings` will flag any miss).

- [ ] **Step 4: Run + fix imports.**

Run: `cargo build -p balanze_cli`
Resolve any unused-import / unresolved-name errors (e.g. `ClaudeOAuthSnapshot`, `UsageEvent`, `OpenAiCosts` are already imported at the top of `main.rs`; `Snapshot` too). Do NOT add `#[allow]`; remove genuinely unused imports.

- [ ] **Step 5: Run the full suite — expect unchanged green**

Run: `cargo test --workspace`
Expected: all green, including `crates/balanze_cli/tests/integration_4quadrant.rs` UNCHANGED (it still exercises the per-crate path; Task 3 adds the compose() parity test). If any existing test fails, the move was not behavior-preserving — fix the move, do not edit the test.

- [ ] **Step 6: Gate + commit**

Run: `cargo fmt --all` then `cargo clippy --workspace --all-targets -- -D warnings`

```bash
git add crates/balanze_cli/
git commit -m "refactor(balanze_cli): build_snapshot delegates to snapshot_composer::compose (no behavior change)"
```

---

### Task 3: Repoint `integration_4quadrant.rs` onto `compose()` (the real parity guard)

**Goal:** Add a `FixtureSources: SnapshotSources` (committed fixtures, zero network) and a test that asserts the output of the SAME `compose()` the CLI runs — so a future divergence is impossible to miss.

**Files:**
- Modify: `crates/balanze_cli/tests/integration_4quadrant.rs` (add `FixtureSources` + one `#[tokio::test]`; keep all existing tests)
- Modify: `crates/balanze_cli/Cargo.toml` (add `tokio` macros/rt to `[dev-dependencies]` if the test crate needs `#[tokio::test]` — `balanze_cli` already depends on `tokio` workspace with `macros`+`rt-multi-thread`, so `#[tokio::test]` is available; only add a dev-dep if compilation says otherwise)

**Acceptance Criteria:**
- [ ] A `FixtureSources` impl: `load_claude_events` reads `tests/fixtures/claude/projects` via the existing `load_fixture_events()` (+ returns `files_scanned`); `fetch_codex_quota` reads the codex fixture; `fetch_openai` returns `Ok(None)`; `fetch_oauth` returns `Err(anyhow!("fixture: no oauth"))` (no network).
- [ ] New `#[tokio::test] async fn compose_parity_against_fixtures` calls `snapshot_composer::compose(&FixtureSources, fixed_now).await` and asserts: `anthropic_api_cost.total_micro_usd > 0`; `anthropic_api_cost.total_event_count == 3`; `claude_jsonl.window.total_events_in_window == 3` (fixed `now` = `2026-05-15T11:02:00Z` as in the existing jsonl test); `codex_quota.primary.used_percent ≈ 17.5`; `openai.is_none()` & `openai_error.is_none()`; `claude_oauth.is_none()` & `claude_oauth_error.is_some()` (oauth deliberately errored in the fixture).
- [ ] All pre-existing tests in the file remain and pass UNCHANGED.
- [ ] `cargo test -p balanze_cli --test integration_4quadrant` green.

**Verify:** `cargo test -p balanze_cli --test integration_4quadrant`

**Steps:**

- [ ] **Step 1: Add `FixtureSources` + the parity test.** Append to `crates/balanze_cli/tests/integration_4quadrant.rs` (keep existing `use` lines; add `use snapshot_composer::{compose, SnapshotSources};` and `use codex_local::{find_latest_session, read_latest_quota_snapshot};` is already imported):

```rust
struct FixtureSources;

impl SnapshotSources for FixtureSources {
    async fn fetch_oauth(
        &self,
    ) -> anyhow::Result<anthropic_oauth::ClaudeOAuthSnapshot> {
        // No network in the integration test: oauth deliberately fails so
        // we also exercise compose()'s now-relative window fallback.
        anyhow::bail!("fixture: no oauth")
    }
    async fn load_claude_events(
        &self,
    ) -> anyhow::Result<(Vec<UsageEvent>, usize)> {
        Ok((load_fixture_events(), 1))
    }
    async fn fetch_codex_quota(
        &self,
    ) -> anyhow::Result<Option<codex_local::CodexQuotaSnapshot>> {
        let codex_dir = fixture_root().join("codex/sessions");
        let path = find_latest_session(&codex_dir)?.expect("fixture session present");
        Ok(read_latest_quota_snapshot(&path)?)
    }
    async fn fetch_openai(&self) -> anyhow::Result<Option<openai_client::OpenAiCosts>> {
        Ok(None)
    }
}

#[tokio::test]
async fn compose_parity_against_fixtures() {
    // Same fixed `now` as `full_pipeline_populates_claude_jsonl_in_snapshot`
    // so all 3 fixture events fall in the 5h window deterministically.
    let now = chrono::DateTime::parse_from_rfc3339("2026-05-15T11:02:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let snap = compose(&FixtureSources, now).await;

    // OAuth deliberately errored ⇒ error slot set, data None, window
    // falls back to now-relative.
    assert!(snap.claude_oauth.is_none());
    assert!(snap.claude_oauth_error.is_some());

    let jsonl = snap.claude_jsonl.as_ref().expect("jsonl populated");
    assert_eq!(jsonl.files_scanned, 1);
    assert_eq!(
        jsonl.window.total_events_in_window, 3,
        "all 3 fixture events fall in the 5h window for the fixed now"
    );

    let cost = snap
        .anthropic_api_cost
        .as_ref()
        .expect("anthropic_api_cost populated");
    assert!(cost.total_micro_usd > 0);
    assert_eq!(cost.total_event_count, 3, "dedup collapses 4 raw → 3");
    assert_eq!(snap.anthropic_api_cost_error, None);

    let codex = snap.codex_quota.as_ref().expect("codex populated");
    assert!((codex.primary.used_percent - 17.5).abs() < 0.001);

    assert!(snap.openai.is_none() && snap.openai_error.is_none());
}
```

- [ ] **Step 2: Run — expect green**

Run: `cargo test -p balanze_cli --test integration_4quadrant`
Expected: the new test plus all pre-existing tests pass. If `anyhow` is not a dev-dep of `balanze_cli`'s test target, it is already a normal dep (so usable from tests) — no Cargo change expected; if the compiler disagrees, add `anyhow = { workspace = true }` to `[dev-dependencies]`.

- [ ] **Step 3: Gate + commit**

Run: `cargo fmt --all` then `cargo clippy --workspace --all-targets -- -D warnings` then `cargo test --workspace`

```bash
git add crates/balanze_cli/
git commit -m "test(balanze_cli): integration parity via snapshot_composer::compose + FixtureSources"
```

---

### Task 4: `backoff` crate — pure policy + generic retry combinator

**Goal:** A dependency-light crate with a pure exponential `BackoffPolicy` (`standard` = 30s×2ⁿ cap 10 min; `fail_fast` = 0 retries) and a generic `retry` combinator, fully unit-tested under paused tokio time (no real sleeps).

**Files:**
- Create: `crates/backoff/Cargo.toml`
- Create: `crates/backoff/src/lib.rs`

**Acceptance Criteria:**
- [ ] `BackoffPolicy::standard()` = base 30s, factor 2, cap 600s, max_retries 6; `fail_fast()` = max_retries 0; `custom(base, factor, cap, max_retries)`.
- [ ] `delay_for_attempt(n)` = `min(base * factor^n, cap)`, saturating (no overflow panic for large `n`).
- [ ] `RetryDecision::{DoNotRetry, RetryAfter(Option<Duration>)}`.
- [ ] `retry(policy, classify, op)` returns `Ok` immediately on success; on `Err` → `DoNotRetry` returns it at once; `RetryAfter(_)` retries after `server.map_or(schedule, |d| min(d, cap))`, up to `max_retries` then returns the last error.
- [ ] Tests under `#[tokio::test(start_paused = true)]` assert: `fail_fast` calls `op` exactly once; a 3-fail-then-ok op under `standard` succeeds after exactly 3 retries; `DoNotRetry` returns after exactly one call; a server `RetryAfter(Some(5s))` sleeps ~5s (assert via `tokio::time::Instant` delta); `delay_for_attempt` math incl. saturation at large `n`.
- [ ] `cargo clippy -p backoff --all-targets -- -D warnings` clean.

**Verify:** `cargo test -p backoff && cargo clippy -p backoff --all-targets -- -D warnings`

**Steps:**

- [ ] **Step 1: `crates/backoff/Cargo.toml`**

```toml
[package]
name = "backoff"
description = "Pure exponential-backoff policy + a generic async retry combinator. No HTTP/provider knowledge — callers classify their own errors. AGENTS.md §3.1."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
publish.workspace = true

[dependencies]
tokio = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros", "rt"] }
```

- [ ] **Step 2: Write failing tests first** — create `crates/backoff/src/lib.rs` with the test module first (compile-fails until Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[test]
    fn delay_schedule_and_saturation() {
        let p = BackoffPolicy::standard();
        assert_eq!(p.delay_for_attempt(0), Duration::from_secs(30));
        assert_eq!(p.delay_for_attempt(1), Duration::from_secs(60));
        assert_eq!(p.delay_for_attempt(2), Duration::from_secs(120));
        assert_eq!(p.delay_for_attempt(4), Duration::from_secs(480));
        assert_eq!(p.delay_for_attempt(5), Duration::from_secs(600)); // capped
        assert_eq!(p.delay_for_attempt(99), Duration::from_secs(600)); // saturates, no panic
    }

    #[tokio::test(start_paused = true)]
    async fn fail_fast_calls_op_once_and_returns_err() {
        let calls = AtomicU32::new(0);
        let r: Result<(), &str> = retry(
            &BackoffPolicy::fail_fast(),
            |_| RetryDecision::RetryAfter(None),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("boom") }
            },
        )
        .await;
        assert_eq!(r, Err("boom"));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fail_fast ⇒ no retries");
    }

    #[tokio::test(start_paused = true)]
    async fn retries_then_succeeds_under_standard() {
        let calls = AtomicU32::new(0);
        let r: Result<u32, &str> = retry(
            &BackoffPolicy::standard(),
            |_| RetryDecision::RetryAfter(None),
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 3 {
                        Err("transient")
                    } else {
                        Ok(n)
                    }
                }
            },
        )
        .await;
        assert_eq!(r, Ok(3));
        assert_eq!(calls.load(Ordering::SeqCst), 4, "3 retries then success");
    }

    #[tokio::test(start_paused = true)]
    async fn do_not_retry_returns_immediately() {
        let calls = AtomicU32::new(0);
        let r: Result<(), &str> = retry(
            &BackoffPolicy::standard(),
            |_| RetryDecision::DoNotRetry,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("fatal") }
            },
        )
        .await;
        assert_eq!(r, Err("fatal"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn server_retry_after_is_honored_and_capped() {
        let start = tokio::time::Instant::now();
        let calls = AtomicU32::new(0);
        let _ = retry::<(), &str, _, _>(
            &BackoffPolicy::standard(),
            |_| RetryDecision::RetryAfter(Some(Duration::from_secs(5))),
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Err("rate limited")
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;
        // One retry, slept the server-suggested 5s (not the 30s schedule).
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }
}
```

- [ ] **Step 3: Implement** — prepend to `crates/backoff/src/lib.rs`:

```rust
//! Pure exponential-backoff policy + a generic async retry combinator.
//! No HTTP/provider types here — the caller supplies a `classify` closure
//! that inspects its own error. AGENTS.md §3.1 (start 30s, cap 10 min).
//! Single-user tool ⇒ no jitter (no thundering herd to spread).

use std::time::Duration;

/// Exponential-backoff schedule + retry budget.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    base: Duration,
    factor: u32,
    cap: Duration,
    max_retries: u32,
}

impl BackoffPolicy {
    /// §3.1 background-poller schedule: 30s, 60s, 120s, … capped at 10 min,
    /// 6 retries. For the future watcher's safety poll — NOT the one-shot CLI.
    pub fn standard() -> Self {
        Self {
            base: Duration::from_secs(30),
            factor: 2,
            cap: Duration::from_secs(600),
            max_retries: 6,
        }
    }

    /// One-shot CLI: never block a user-facing invocation on provider
    /// rate-limit backoff. Zero retries — surface the error immediately.
    pub fn fail_fast() -> Self {
        Self {
            base: Duration::from_secs(0),
            factor: 2,
            cap: Duration::from_secs(0),
            max_retries: 0,
        }
    }

    pub fn custom(base: Duration, factor: u32, cap: Duration, max_retries: u32) -> Self {
        Self {
            base,
            factor,
            cap,
            max_retries,
        }
    }

    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    pub fn cap(&self) -> Duration {
        self.cap
    }

    /// `min(base * factor^attempt, cap)`, saturating (no overflow panic).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let mult = (self.factor as u64).checked_pow(attempt).unwrap_or(u64::MAX);
        let secs = self.base.as_secs().checked_mul(mult).unwrap_or(u64::MAX);
        let d = Duration::from_secs(secs);
        if d > self.cap {
            self.cap
        } else {
            d
        }
    }
}

/// What `retry` should do with a given error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Permanent failure — return it immediately.
    DoNotRetry,
    /// Transient — retry. `Some(d)` = server-suggested delay (e.g. parsed
    /// `Retry-After`), used instead of the schedule (clamped to the cap).
    /// `None` = use the policy schedule.
    RetryAfter(Option<Duration>),
}

/// Run `op`, retrying transient failures per `policy`. Pure scheduling +
/// `tokio::time::sleep`; deterministic under `tokio::time::pause()`.
pub async fn retry<T, E, Op, Fut>(
    policy: &BackoffPolicy,
    classify: impl Fn(&E) -> RetryDecision,
    mut op: Op,
) -> Result<T, E>
where
    Op: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => match classify(&e) {
                RetryDecision::DoNotRetry => return Err(e),
                RetryDecision::RetryAfter(_) if attempt >= policy.max_retries => return Err(e),
                RetryDecision::RetryAfter(server) => {
                    let delay = match server {
                        Some(d) => d.min(policy.cap()),
                        None => policy.delay_for_attempt(attempt),
                    };
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            },
        }
    }
}
```

- [ ] **Step 4: Run — expect green**

Run: `cargo test -p backoff`
Expected: all 5 tests pass (paused time auto-advances; no real waiting).

- [ ] **Step 5: Gate + commit**

Run: `cargo fmt --all` then `cargo clippy -p backoff --all-targets -- -D warnings`

```bash
git add crates/backoff/
git commit -m "feat(backoff): pure exponential-backoff policy + generic retry combinator"
```

---

### Task 5: Wire `backoff` into `anthropic_oauth` (CLI passes fail-fast)

**Goal:** `fetch_usage` and `refresh_access_token` accept a `&BackoffPolicy`, retry transient/429/5xx via `backoff::retry` (honoring `Retry-After`), never retry 401 (it must keep flowing to Track A's refresh path). The CLI's `LiveSources` passes `BackoffPolicy::fail_fast()`. Workspace stays compiling per-commit.

**Files:**
- Modify: `crates/anthropic_oauth/Cargo.toml` (add `backoff` path dep)
- Modify: `crates/anthropic_oauth/src/types.rs` (add `OAuthError::RateLimited { retry_after: Option<Duration> }`)
- Modify: `crates/anthropic_oauth/src/client.rs` (`fetch_usage` signature + retry wrap + capture 429 `Retry-After`)
- Modify: `crates/anthropic_oauth/src/refresh.rs` (`refresh_access_token` signature + retry wrap)
- Modify: `crates/anthropic_oauth/tests/refresh_wiremock.rs` (call-site updates + 429-retry + 401-no-retry tests)
- Modify: `crates/balanze_cli/src/main.rs` (`live_fetch_oauth` + `refresh_and_persist` pass `&BackoffPolicy::fail_fast()`)

**Acceptance Criteria:**
- [ ] `pub async fn fetch_usage(client, base_url, access_token, subscription_type, rate_limit_tier, policy: &backoff::BackoffPolicy) -> Result<ClaudeOAuthSnapshot, OAuthError>` — new last param.
- [ ] `pub async fn refresh_access_token(client, token_url, client_id, refresh_token, now_ms, policy: &backoff::BackoffPolicy) -> Result<RefreshedTokens, OAuthError>` — new last param.
- [ ] HTTP 429 → `OAuthError::RateLimited { retry_after }` where `retry_after` is the parsed `Retry-After` header (`Some(Duration)`) or `None`. The classifier maps `RateLimited` → `RetryAfter(retry_after)`, `Network` (transport/timeout) → `RetryAfter(None)`, `UnexpectedStatus{status: 500..=599}` → `RetryAfter(None)`, everything else (incl. `AuthExpired`, `RefreshFailed`, `ResponseShape`, `RefreshTokenMissing`, `UnexpectedStatus` non-5xx) → `DoNotRetry`.
- [ ] A 401 still surfaces as `OAuthError::AuthExpired` with **no** retry (verified by a wiremock test that asserts the mock was hit exactly once).
- [ ] A 429-then-200 wiremock test passes with `BackoffPolicy::custom(Duration::ZERO, 2, Duration::ZERO, 3)` (zero-delay so the test is instant) and asserts the snapshot parses (one retry happened).
- [ ] CLI: `live_fetch_oauth` and `refresh_and_persist` pass `&backoff::BackoffPolicy::fail_fast()` to the two fns.
- [ ] `cargo test -p anthropic_oauth` + `cargo test --workspace` green; clippy `-D warnings` clean.

**Verify:** `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`

**Steps:**

- [ ] **Step 1: Cargo dep.** `crates/anthropic_oauth/Cargo.toml` `[dependencies]`: add `backoff = { path = "../backoff" }`.

- [ ] **Step 2: New error variant.** In `crates/anthropic_oauth/src/types.rs`, add to the `OAuthError` enum (after `RefreshFailed`):

```rust
    #[error("rate limited by Anthropic (HTTP 429){}", match retry_after {
        Some(d) => format!("; retry after {}s", d.as_secs()),
        None => String::new(),
    })]
    RateLimited { retry_after: Option<std::time::Duration> },
```

- [ ] **Step 3: Write the failing wiremock tests.** In `crates/anthropic_oauth/tests/refresh_wiremock.rs` add (the existing tests there call `refresh_access_token` — update ALL existing call sites to pass a 6th arg `&backoff::BackoffPolicy::fail_fast()` so the file compiles; add `use std::time::Duration;` if not present):

```rust
#[tokio::test]
async fn fetch_usage_retries_on_429_then_succeeds() {
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};
    let server = MockServer::start().await;
    // First call → 429, second → 200 (wiremock `up_to_n_times` + scenario).
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = reqwest::Client::new();
    let zero = backoff::BackoffPolicy::custom(std::time::Duration::ZERO, 2, std::time::Duration::ZERO, 3);
    let out = anthropic_oauth::fetch_usage(
        &client, &server.uri(), "tok", None, None, &zero,
    )
    .await;
    assert!(out.is_ok(), "should succeed after one 429 retry: {out:?}");
}

#[tokio::test]
async fn fetch_usage_401_does_not_retry() {
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};
    let server = MockServer::start().await;
    let mock = Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1) // exactly once — no retry on auth failure
        .named("oauth 401");
    server.register(mock).await;
    let client = reqwest::Client::new();
    let std_pol = backoff::BackoffPolicy::standard();
    let out = anthropic_oauth::fetch_usage(
        &client, &server.uri(), "tok", None, None, &std_pol,
    )
    .await;
    assert!(matches!(out, Err(anthropic_oauth::OAuthError::AuthExpired)));
    // server drop verifies `.expect(1)`.
}
```

- [ ] **Step 4: Implement the retry wrap in `client.rs`.** Change `fetch_usage`'s signature to add `policy: &backoff::BackoffPolicy` as the final param. Restructure so the single HTTP attempt is a closure and the status mapping produces `RateLimited` for 429 (parse `Retry-After`: `resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).map(std::time::Duration::from_secs)`), then wrap it:

```rust
let classify = |e: &OAuthError| match e {
    OAuthError::RateLimited { retry_after } => backoff::RetryDecision::RetryAfter(*retry_after),
    OAuthError::Network(_) => backoff::RetryDecision::RetryAfter(None),
    OAuthError::UnexpectedStatus { status, .. } if (500..=599).contains(status) => {
        backoff::RetryDecision::RetryAfter(None)
    }
    _ => backoff::RetryDecision::DoNotRetry, // AuthExpired etc. → caller's refresh path
};
backoff::retry(policy, classify, || async { do_one_fetch_usage_attempt(...).await }).await
```
Keep the existing 200/401/other mapping; add the `429 ⇒ RateLimited { retry_after }` arm BEFORE the catch-all `_ ⇒ UnexpectedStatus`. The 200 success path, `redact_for_display`, and parse logic are unchanged. `AuthExpired` must remain returned verbatim so `balanze_cli`'s existing 401→refresh→retry-once still works.

- [ ] **Step 5: Same wrap in `refresh.rs`.** `refresh_access_token` gains the final `policy: &backoff::BackoffPolicy` param; wrap its single attempt with `backoff::retry` and a classifier mapping `RefreshFailed { status, .. }` with `status == 429` → `RetryAfter` (parse `Retry-After` if you keep the response; if the current code discards it, add a `RateLimited` arm analogous to Step 4), `Network` → `RetryAfter(None)`, 5xx `RefreshFailed` → `RetryAfter(None)`, everything else → `DoNotRetry`. The redaction added in Track A stays.

- [ ] **Step 6: Update CLI call sites.** In `crates/balanze_cli/src/main.rs`: `live_fetch_oauth` builds `let policy = backoff::BackoffPolicy::fail_fast();` and passes `&policy` to both `fetch_usage(...)` calls; `refresh_and_persist` passes `&backoff::BackoffPolicy::fail_fast()` to `refresh_access_token(...)`. Add `backoff = { path = "../backoff" }` to `crates/balanze_cli/Cargo.toml`. (Rationale comment: "one-shot CLI must not block on provider backoff; the watcher will pass `standard()`.")

- [ ] **Step 7: Run + commit**

Run: `cargo test --workspace` (expect green incl. the 2 new wiremock tests), then `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`.

```bash
git add crates/anthropic_oauth/ crates/balanze_cli/
git commit -m "feat(anthropic_oauth): backoff/429 retry layer (CLI passes fail-fast)"
```

---

### Task 6: Wire `backoff` into `openai_client` (CLI passes fail-fast)

**Goal:** `fetch_costs`/`costs_this_month` accept a `&BackoffPolicy`, retry transient/429/5xx, never retry 401/403. CLI passes `fail_fast()`. Closes the §3.1 backoff gap for both HTTP clients.

**Files:**
- Modify: `crates/openai_client/Cargo.toml` (add `backoff` path dep)
- Modify: `crates/openai_client/src/types.rs` (add `OpenAiError::RateLimited { retry_after: Option<Duration> }`)
- Modify: `crates/openai_client/src/client.rs` (`fetch_costs` + `costs_this_month` signatures + retry wrap + 429 `Retry-After`)
- Modify: `crates/openai_client/tests/wiremock_tests.rs` (call-site updates + 429-retry + 403-no-retry tests)
- Modify: `crates/balanze_cli/src/main.rs` (`live_fetch_openai` passes `&BackoffPolicy::fail_fast()`)

**Acceptance Criteria:**
- [ ] `pub async fn costs_this_month(client, base_url, admin_key, policy: &backoff::BackoffPolicy)` and `pub async fn fetch_costs(client, base_url, admin_key, start_time, end_time, policy: &backoff::BackoffPolicy)` — new final param.
- [ ] 429 → `OpenAiError::RateLimited { retry_after }`; classifier: `RateLimited` → `RetryAfter(retry_after)`, `Network` → `RetryAfter(None)`, `UnexpectedStatus{500..=599}` → `RetryAfter(None)`, `AuthInvalid`/`InsufficientScope`/`ResponseShape`/non-5xx `UnexpectedStatus` → `DoNotRetry`.
- [ ] 429-then-200 wiremock test passes with a zero-delay `custom` policy; a 403 wiremock test asserts the mock is hit exactly once (no retry) and returns `InsufficientScope`.
- [ ] CLI `live_fetch_openai` passes `&backoff::BackoffPolicy::fail_fast()`.
- [ ] `cargo test --workspace` green; clippy `-D warnings` clean; `cargo fmt --all -- --check` clean.

**Verify:** `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`

**Steps:**

- [ ] **Step 1: Cargo dep.** `crates/openai_client/Cargo.toml` `[dependencies]`: `backoff = { path = "../backoff" }`.

- [ ] **Step 2: Error variant.** `crates/openai_client/src/types.rs`, add to `OpenAiError` (after `UnexpectedStatus`):

```rust
    #[error("rate limited by OpenAI (HTTP 429){}", match retry_after {
        Some(d) => format!("; retry after {}s", d.as_secs()),
        None => String::new(),
    })]
    RateLimited { retry_after: Option<std::time::Duration> },
```

- [ ] **Step 3: Failing wiremock tests.** In `crates/openai_client/tests/wiremock_tests.rs`: update ALL existing `costs_this_month`/`fetch_costs` call sites to pass a final `&backoff::BackoffPolicy::fail_fast()` (so the file compiles), then add:

```rust
#[tokio::test]
async fn costs_retry_on_429_then_succeed() {
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(429)).up_to_n_times(1)
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"object":"page","data":[],"has_more":false}"#))
        .mount(&server).await;
    let client = reqwest::Client::new();
    let zero = backoff::BackoffPolicy::custom(std::time::Duration::ZERO, 2, std::time::Duration::ZERO, 3);
    let out = openai_client::costs_this_month(&client, &server.uri(), "sk-admin-x", &zero).await;
    assert!(out.is_ok(), "should succeed after one 429 retry: {out:?}");
}

#[tokio::test]
async fn costs_403_does_not_retry() {
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};
    let server = MockServer::start().await;
    let mock = Mock::given(method("GET")).and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(403)).expect(1).named("openai 403");
    server.register(mock).await;
    let client = reqwest::Client::new();
    let std_pol = backoff::BackoffPolicy::standard();
    let out = openai_client::costs_this_month(&client, &server.uri(), "sk-admin-x", &std_pol).await;
    assert!(matches!(out, Err(openai_client::OpenAiError::InsufficientScope { .. })));
}
```

- [ ] **Step 4: Implement.** `fetch_costs` gains the final `policy: &backoff::BackoffPolicy`; `costs_this_month` gains it and forwards. Extract the single send+map into a closure; add a `429 ⇒ RateLimited { retry_after }` arm (parse `Retry-After` like Task 5 Step 4) BEFORE the `_ ⇒ UnexpectedStatus` arm; wrap with `backoff::retry` + the classifier from the AC. The `200`/`401`/`403`/parse paths and `redact_for_display` are unchanged.

- [ ] **Step 5: CLI call site.** `live_fetch_openai` in `crates/balanze_cli/src/main.rs`: pass `&backoff::BackoffPolicy::fail_fast()` to `costs_this_month(...)`. (`backoff` dep already added to `balanze_cli` in Task 5.)

- [ ] **Step 6: Final gates + commit**

Run: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (all green).

```bash
git add crates/openai_client/ crates/balanze_cli/
git commit -m "feat(openai_client): backoff/429 retry layer (CLI passes fail-fast)"
```

---

### Task 7: Docs — AGENTS.md Repo Map / boundaries + prd.md

**Goal:** Keep the authoritative docs truthful: two new crates on the Repo Map allowlist, boundary #8 marked extracted, the §2 YAGNI allowlist note updated, and a one-line prd note that Track B shipped.

**Files:**
- Modify: `AGENTS.md` (Repo Map block; §2 YAGNI line; §4 boundary #8)
- Modify: `docs/prd.md` (Phase 2 Track B note)

**Acceptance Criteria:**
- [ ] Repo Map gains two lines: `snapshot_composer/` (shared compose() policy; the single composition path, §4 #8) and `backoff/` (pure exponential-backoff policy + retry combinator; §3.1). Placed sensibly in the `crates/` listing.
- [ ] §2 YAGNI line that enumerates the crate allowlist is updated so it no longer reads as if `predictor`+`watcher` are the only planned additions — it now reflects `snapshot_composer` + `backoff` as shipped (and `predictor`+`watcher` still planned).
- [ ] §4 boundary #8 "Open tech debt" paragraph is updated: the extraction is DONE — `snapshot_composer::compose` is the shared path; the CLI uses `LiveSources`, the watcher will provide its own `SnapshotSources`. Remove the now-obsolete "Not extracted in v0.1 ... YAGNI" sentence; keep the parity invariant statement.
- [ ] `docs/prd.md` Phase 2 Track B paragraph gets a one-line "shipped: composer extracted; backoff layer added (CLI fail-fast, watcher will use standard)".
- [ ] No unrelated doc lines reflowed (markdown has no column cap per §2.1 — do not rewrap).

**Verify:** `git diff --stat AGENTS.md docs/prd.md` shows only the intended hunks; `rg -n "snapshot_composer|crates/backoff" AGENTS.md` shows the new Repo Map lines.

**Steps:**

- [ ] **Step 1:** Read `AGENTS.md` §4 Repo Map fenced block; add the two crate lines next to the existing one-line-purpose entries (keep the established `name/  purpose` column style; do not reflow siblings).
- [ ] **Step 2:** Update the §2 YAGNI sentence ("The crate set is fixed and enumerated in the Repo Map (plus `predictor` + `watcher` still planned for v0.2)") to read that `snapshot_composer` + `backoff` shipped in Track B and `predictor` + `watcher` remain the planned v0.2 additions.
- [ ] **Step 3:** Rewrite the §4 boundary #8 "Open tech debt" paragraph to past tense: the policy is extracted into `snapshot_composer::compose`; `balanze_cli` runs it via `LiveSources`; the watcher/pollers will provide their own `SnapshotSources`; the "identical inputs ⇒ identical Snapshot" invariant now has a single implementation + a fixture parity test (`integration_4quadrant::compose_parity_against_fixtures`). Drop the "Not extracted in v0.1 (YAGNI)" clause.
- [ ] **Step 4:** Add the one-line prd.md Track B "shipped" note (find Phase 2 Track B; append a sentence; do not restructure).
- [ ] **Step 5: Gate + commit** (markdown only — pre-commit rustfmt/clippy skip):

```bash
git add AGENTS.md docs/prd.md
git commit -m "docs: record snapshot_composer + backoff crates; mark §4 #8 extraction done"
```

---

## Self-Review

**Spec coverage:** Step 3 of the brief (extract `build_snapshot` → shared composer crate; repoint `integration_4quadrant.rs`; no behavior change) = Tasks 1–3. Step 4 (backoff/429 layer, 30s×2ⁿ capped, prereq for watcher) = Tasks 4–6. Docs truthfulness (§8 obligation) = Task 7. No gaps.

**Placeholder scan:** No TBD/"handle errors"/"similar to Task N". Verbatim-move steps cite exact source line ranges + "unchanged" — concrete relocation instructions, not placeholders. All new code is given in full.

**Type consistency:** `SnapshotSources` four method signatures are identical across Task 1 (def), Task 2 (`LiveSources` impl), Task 3 (`FixtureSources` impl). `compose<S: SnapshotSources>(&S, DateTime<Utc>) -> Snapshot` consistent. `BackoffPolicy::{standard,fail_fast,custom,delay_for_attempt,cap,max_retries}` + `RetryDecision::{DoNotRetry,RetryAfter(Option<Duration>)}` + `retry(policy, classify, op)` consistent between Task 4 (def) and Tasks 5/6 (callers). `OAuthError::RateLimited`/`OpenAiError::RateLimited { retry_after: Option<Duration> }` consistent between the variant-add step and the classifier step within Tasks 5/6. The `fetch_usage`/`refresh_access_token`/`costs_this_month`/`fetch_costs` new `policy` param is threaded to every call site named in the same task (workspace stays compiling per-commit — the Track A precedent).

**Dependency order:** 1 → 2 → 3 (composer before CLI repoint before parity test); 4 (backoff, independent) → 5 → 6 (clients depend on backoff); 7 docs last. Linear chain is correct for one branch / solo.

## Out of scope (deliberately deferred)

- No poller/watcher is built here — Track B only makes one possible without a tight-loop. The watcher (Track E) supplies its own `SnapshotSources` (likely reusing `LiveSources`-shaped logic) and passes `BackoffPolicy::standard()`.
- `RateLimited` is added to the two HTTP-client error enums only; it is intentionally NOT a new `DegradedState` (no UI yet; §8 surface kept minimal — it maps into the existing `*_error` string slots via `compose`).
- TODO-003 (uniform serde-error redaction) is untouched; unrelated to Track B.
