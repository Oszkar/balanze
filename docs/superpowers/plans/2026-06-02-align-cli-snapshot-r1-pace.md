# Align CLI + Snapshot with the merged model (R1 matrix + pace view) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the code in line with the just-merged roadmap docs: render the matrix as measured-only (R1), replace the EWMA predictor with a pace view (used % vs elapsed % + ratio), and retire the `predictor` crate.

**Architecture:** A new pure `window::pace` primitive computes used/elapsed/ratio for a quota window. A shared `state_coordinator::pace_for_oauth` maps the OAuth cadence bars into `Snapshot.pace` (replacing `Snapshot.prediction`), populated by both the one-shot `compose()` and the live coordinator so the CLI ≡ watcher invariant holds. The CLI compact renderer is reshaped so the API-$ column holds only real billed money; the list-price figure moves to a separate "Subscription leverage" line; a pace line is added. The `predictor` crate and its `--json .prediction` cell are removed (replaced by `.pace`).

**Tech Stack:** Rust 2024, chrono, serde; `cargo test/clippy/fmt`; conventional-commit hooks via lefthook.

**Sequencing rationale:** Task 1 is a standalone pure function. Task 2 *adds* pace everywhere (workspace still compiles, `--json` temporarily carries both `.prediction` and `.pace`). Task 3 *removes* the predictor (workspace compiles again). Task 4 is presentation-only and depends on the `pace` field from Task 2. Task 5 is docs.

**Conventions (from AGENTS.md):** `i64` micro-USD; pure crates (`window`) get tests written **before** impl; no `.unwrap()` outside tests; `cargo clippy --workspace --all-targets -- -D warnings` must pass; conventional-commit messages; never `--no-verify`. Code comments stay free of release/track labels (the cleanup just landed).

---

### Task 1: Add pure `pace` computation to the `window` crate

**Goal:** A pure, tested `window::pace(...)` that returns used/elapsed fractions and a `used ÷ elapsed` ratio for a quota window.

**Files:**
- Modify: `crates/window/src/lib.rs` (add a `SEVEN_DAY_WINDOW` const, a `Pace` struct, a `pace` fn; add tests in the existing `#[cfg(test)] mod tests`)

**Acceptance Criteria:**
- [ ] `window::pace` is pure (no I/O), takes `(used_percent: f64, resets_at, window_len, now)` and returns `Pace { used_fraction, elapsed_fraction, ratio }`.
- [ ] `ratio` is `None` when `elapsed_fraction == 0.0` (right after a reset — no honest verdict yet); `elapsed_fraction` is clamped to `[0.0, 1.0]`; `used_fraction` is **not** clamped (can exceed 1.0 over cap).
- [ ] New tests cover: ahead-of-pace, on-track, just-after-reset (ratio `None`), past-reset clamp, over-cap usage.

**Verify:** `cargo test -p window` → all pass; `cargo clippy -p window --all-targets -- -D warnings` → clean.

**Steps:**

- [ ] **Step 1: Write the failing tests.** Add to the bottom of the `#[cfg(test)] mod tests { ... }` block in `crates/window/src/lib.rs`:

```rust
    // --- pace ---
    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn pace_ahead_of_linear() {
        // 2h into a 5h window (40% elapsed), 82% used → ratio ≈ 2.05.
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now + Duration::hours(3); // window_start = now - 2h
        let p = pace(82.0, resets_at, DEFAULT_WINDOW, now);
        assert!((p.used_fraction - 0.82).abs() < 1e-9);
        assert!((p.elapsed_fraction - 0.40).abs() < 1e-9);
        assert!((p.ratio.unwrap() - 2.05).abs() < 1e-6);
    }

    #[test]
    fn pace_on_track() {
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now + Duration::hours(3);
        let p = pace(40.0, resets_at, DEFAULT_WINDOW, now);
        assert!((p.ratio.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pace_just_after_reset_has_no_ratio() {
        // window_start == now → 0 elapsed → no honest verdict.
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now + DEFAULT_WINDOW;
        let p = pace(0.5, resets_at, DEFAULT_WINDOW, now);
        assert_eq!(p.elapsed_fraction, 0.0);
        assert_eq!(p.ratio, None);
    }

    #[test]
    fn pace_past_reset_clamps_elapsed_to_one() {
        // now is past resets_at (stale data) → elapsed clamped to 1.0, not >1.
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now - Duration::hours(1);
        let p = pace(90.0, resets_at, DEFAULT_WINDOW, now);
        assert_eq!(p.elapsed_fraction, 1.0);
        assert!((p.ratio.unwrap() - 0.90).abs() < 1e-9);
    }

    #[test]
    fn pace_over_cap_used_not_clamped() {
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now + Duration::hours(3);
        let p = pace(120.0, resets_at, DEFAULT_WINDOW, now);
        assert!((p.used_fraction - 1.20).abs() < 1e-9);
    }

    #[test]
    fn pace_seven_day_window_length() {
        // 7d window: 7 days = 168h; 3.5 days in → 50% elapsed.
        let now = t("2026-06-02T12:00:00Z");
        let resets_at = now + Duration::days(7) - Duration::days(3) - Duration::hours(12);
        let p = pace(25.0, resets_at, SEVEN_DAY_WINDOW, now);
        assert!((p.elapsed_fraction - 0.5).abs() < 1e-9);
        assert!((p.ratio.unwrap() - 0.5).abs() < 1e-9);
    }
```

- [ ] **Step 2: Run the tests, confirm they fail to compile** (`pace`, `Pace`, `SEVEN_DAY_WINDOW` undefined).

Run: `cargo test -p window pace`
Expected: compile error — `cannot find function `pace`` / `cannot find type `Pace``.

- [ ] **Step 3: Add the const, struct, and function.** In `crates/window/src/lib.rs`, near the existing constants (after `DEFAULT_MIN_BURN_EVENTS`) add the const, and after the `WindowSummary` struct add `Pace` + `pace`:

```rust
/// Length of the Claude "7-day" rolling quota window. (`DEFAULT_WINDOW` is the
/// 5-hour window.) Used by `pace` to turn a cadence's `resets_at` into an
/// elapsed fraction.
pub const SEVEN_DAY_WINDOW: Duration = Duration::days(7);
```

```rust
/// Where a quota window stands right now: how much is used vs. how much of the
/// window's wall-clock has elapsed. The honest replacement for a forward
/// prediction — two measured facts plus their ratio, no forecast.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pace {
    /// Fraction of the quota consumed (`used_percent / 100`). NOT clamped —
    /// can exceed 1.0 when an account is over cap.
    pub used_fraction: f64,
    /// Fraction of the window's wall-clock elapsed, clamped to `[0.0, 1.0]`.
    pub elapsed_fraction: f64,
    /// `used_fraction / elapsed_fraction` — > 1.0 means burning faster than a
    /// linear pace, < 1.0 means comfortably behind. `None` right after a reset
    /// (no time elapsed yet), where no honest verdict is possible.
    pub ratio: Option<f64>,
}

/// Compute the current pace of a quota window from its server-reported
/// utilization and reset time. Pure: a function of `(used_percent, resets_at,
/// window_len, now)` only — no warm-up state, no history, never lies after a
/// reset (it just reports `ratio: None` until the clock moves).
pub fn pace(
    used_percent: f64,
    resets_at: DateTime<Utc>,
    window_len: Duration,
    now: DateTime<Utc>,
) -> Pace {
    let used_fraction = used_percent / 100.0;
    let window_start = resets_at - window_len;
    let window_secs = window_len.num_seconds() as f64;
    let elapsed_secs = (now - window_start).num_seconds() as f64;
    let elapsed_fraction = if window_secs > 0.0 {
        (elapsed_secs / window_secs).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let ratio = if elapsed_fraction > 0.0 {
        Some(used_fraction / elapsed_fraction)
    } else {
        None
    };
    Pace {
        used_fraction,
        elapsed_fraction,
        ratio,
    }
}
```

- [ ] **Step 4: Run tests + clippy.**

Run: `cargo test -p window && cargo clippy -p window --all-targets -- -D warnings`
Expected: all tests PASS; clippy clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/window/src/lib.rs
git commit -m "feat(window): add pure pace() — used vs elapsed fraction + ratio"
```

---

### Task 2: Add `pace` to `Snapshot` and populate it (predictor still present)

**Goal:** Add `Snapshot.pace: Vec<WindowPace>` plus a shared `pace_for_oauth` glue, populate it from both compose paths, and surface it in `--json` as `.pace`. `prediction` stays for now so the workspace keeps compiling.

**Files:**
- Modify: `crates/state_coordinator/src/snapshot.rs` (add `WindowPace`, `pace_for_oauth`, the `pace` field, `empty()`)
- Modify: `crates/state_coordinator/src/lib.rs` (re-export `WindowPace`, `pace_for_oauth`)
- Modify: `crates/state_coordinator/src/coordinator.rs` (set `pace` after a merge with OAuth present)
- Modify: `crates/snapshot_composer/src/lib.rs` (set `pace` in `compose()`)
- Modify: `crates/balanze_cli/src/json_output.rs` (add `JsonPace`, `.pace` field + mapping)

**Acceptance Criteria:**
- [ ] `Snapshot` has `pub pace: Vec<WindowPace>`; `empty()` initializes it to `Vec::new()`.
- [ ] `pace_for_oauth(&ClaudeOAuthSnapshot, now)` returns one `WindowPace` per cadence whose key maps to a known window length (`five_hour` → 5h, `seven_day` → 7d); unknown keys are skipped.
- [ ] `compose()` and the coordinator both populate `pace` from the same `pace_for_oauth` (CLI ≡ watcher).
- [ ] `--json` emits a `.pace` array (kept alongside `.prediction` for this task).

**Verify:** `cargo test --workspace` → pass; `cargo clippy --workspace --all-targets -- -D warnings` → clean. Manual: `cargo run -p balanze_cli -- status --json | jq '.pace'` shows the array when OAuth data is present.

**Steps:**

- [ ] **Step 1: Add `WindowPace` + `pace_for_oauth` + the field in `snapshot.rs`.** At the top of `crates/state_coordinator/src/snapshot.rs`, extend the `window` import and add `chrono::Duration`:

```rust
use chrono::{DateTime, Duration, Utc};
use window::{DEFAULT_WINDOW, Pace, SEVEN_DAY_WINDOW, WindowSummary, pace};
```

Add the type + glue (after the imports, before `Snapshot`):

```rust
/// Per-window pace, mirrored from the OAuth cadence bars. Replaces the retired
/// forward predictor: measured used % vs elapsed % of the window, plus their
/// ratio. One entry per cadence whose window length is known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowPace {
    /// Cadence key, e.g. `"five_hour"` / `"seven_day"`.
    pub key: String,
    pub used_fraction: f64,
    pub elapsed_fraction: f64,
    pub ratio: Option<f64>,
}

fn window_len_for(key: &str) -> Option<Duration> {
    match key {
        "five_hour" => Some(DEFAULT_WINDOW),
        "seven_day" => Some(SEVEN_DAY_WINDOW),
        _ => None,
    }
}

/// Map the OAuth cadence bars into per-window pace. Shared by `compose()` (CLI)
/// and the coordinator (watcher) so the two paths cannot diverge.
pub fn pace_for_oauth(oauth: &ClaudeOAuthSnapshot, now: DateTime<Utc>) -> Vec<WindowPace> {
    oauth
        .cadences
        .iter()
        .filter_map(|c| {
            let len = window_len_for(&c.key)?;
            let p: Pace = pace(c.utilization_percent as f64, c.resets_at, len, now);
            Some(WindowPace {
                key: c.key.clone(),
                used_fraction: p.used_fraction,
                elapsed_fraction: p.elapsed_fraction,
                ratio: p.ratio,
            })
        })
        .collect()
}
```

In the `Snapshot` struct, **add** (leave `prediction` in place for this task) after the `prediction` field:

```rust
    /// Per-window pace (used vs elapsed) derived from the OAuth cadence bars.
    /// Empty until an OAuth snapshot with a known cadence is present.
    pub pace: Vec<WindowPace>,
```

In `empty()`, after `prediction: None,` add:

```rust
            pace: Vec::new(),
```

- [ ] **Step 2: Re-export from `lib.rs`.** In `crates/state_coordinator/src/lib.rs`, near the existing `pub use predictor::...` line, add:

```rust
pub use snapshot::{WindowPace, pace_for_oauth};
```

(Adjust the module path if `snapshot` items are already re-exported there — match the existing `pub use snapshot::{...}` if present.)

- [ ] **Step 3: Populate `pace` in `compose()`.** In `crates/snapshot_composer/src/lib.rs`, extend the `state_coordinator` import to include `pace_for_oauth`, and in the `Snapshot { ... }` literal returned by `compose()` add (right after `prediction: None,`):

```rust
        pace: claude_oauth
            .as_ref()
            .map(|o| pace_for_oauth(o, now))
            .unwrap_or_default(),
```

- [ ] **Step 4: Populate `pace` in the coordinator.** In `crates/state_coordinator/src/coordinator.rs`, in `handle_msg` right after the existing `maybe_recompute_prediction(...)` call, add a pace recompute. Add this helper next to `maybe_recompute_prediction`:

```rust
/// Recompute `snapshot.pace` from the current OAuth cadence bars after a merge.
/// Empty when no OAuth snapshot is present.
fn recompute_pace(state: &mut CoordinatorState) {
    state.snapshot.pace = state
        .snapshot
        .claude_oauth
        .as_ref()
        .map(|o| pace_for_oauth(o, Utc::now()))
        .unwrap_or_default();
}
```

and call it in `handle_msg` immediately after `maybe_recompute_prediction(state, source);`:

```rust
    recompute_pace(state);
```

Add `pace_for_oauth` to the `use` at the top of `coordinator.rs` (it lives in the same crate — `use crate::snapshot::pace_for_oauth;` or extend the existing `crate::` import).

- [ ] **Step 5: Add `JsonPace` to `--json`.** In `crates/balanze_cli/src/json_output.rs`, add the DTO (near `JsonPrediction`):

```rust
#[derive(Serialize)]
struct JsonPace {
    key: String,
    used_fraction: f64,
    elapsed_fraction: f64,
    ratio: Option<f64>,
    source: &'static str,
}

impl From<&state_coordinator::WindowPace> for JsonPace {
    fn from(p: &state_coordinator::WindowPace) -> Self {
        Self {
            key: p.key.clone(),
            used_fraction: p.used_fraction,
            elapsed_fraction: p.elapsed_fraction,
            ratio: p.ratio,
            source: "window_pace",
        }
    }
}
```

In the `JsonDoc` struct add (after `prediction`):

```rust
    pace: Vec<JsonPace>,
```

In `JsonDoc::from_snapshot`, after the `prediction:` line add:

```rust
            pace: snap.pace.iter().map(JsonPace::from).collect(),
```

- [ ] **Step 6: Run gates.**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all PASS / clean.

- [ ] **Step 7: Commit.**

```bash
git add crates/state_coordinator crates/snapshot_composer crates/balanze_cli/src/json_output.rs
git commit -m "feat: add Snapshot.pace (used vs elapsed) + --json .pace, shared across CLI and watcher"
```

---

### Task 3: Retire the `predictor` crate

**Goal:** Remove `prediction` from `Snapshot`, delete the `predictor` crate and all its references, and drop the now-dead EWMA history ring + `JsonPrediction`. Workspace compiles with pace as the only forward-looking signal.

**Files:**
- Delete: `crates/predictor/` (whole directory)
- Modify: `Cargo.toml` (remove the `predictor` workspace dependency)
- Modify: `crates/state_coordinator/Cargo.toml` (remove `predictor` dep)
- Modify: `crates/state_coordinator/src/lib.rs` (remove the `pub use predictor::...` re-export)
- Modify: `crates/state_coordinator/src/snapshot.rs` (remove `use predictor::Prediction;`, the `prediction` field, `prediction: None` in `empty()`)
- Modify: `crates/state_coordinator/src/coordinator.rs` (remove `maybe_recompute_prediction`, the `predict`/`WindowSnapshot` imports, the `state.history` ring + `HISTORY_CAPACITY`)
- Modify: `crates/snapshot_composer/src/lib.rs` (remove `prediction: None,` from the `compose()` literal)
- Modify: `crates/balanze_cli/src/json_output.rs` (remove `JsonPrediction`, the `prediction` field + mapping, the `predictor::` import)

**Acceptance Criteria:**
- [ ] `crates/predictor/` no longer exists; no `Cargo.toml` references it; `grep -rn "predictor" crates/ src-tauri/ Cargo.toml` returns nothing in code (doc/changelog handled in Task 5).
- [ ] `Snapshot` no longer has a `prediction` field; `grep -rn "\.prediction\b\|prediction:" crates/` returns nothing (except the new `pace`).
- [ ] `cargo build --workspace` compiles; the coordinator no longer keeps a `WindowSnapshot` history.

**Verify:** `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` → all pass/clean.

**Steps:**

- [ ] **Step 1: Remove the `prediction` field + predictor import from `snapshot.rs`.** Delete `use predictor::Prediction;`, delete the `prediction` field (and its doc comment) from `Snapshot`, and delete `prediction: None,` from `empty()`.

- [ ] **Step 2: Strip predictor from the coordinator.** In `crates/state_coordinator/src/coordinator.rs`:
  - Delete the `maybe_recompute_prediction` function in full.
  - Delete its call site in `handle_msg` (`maybe_recompute_prediction(state, source);`) — keep the `recompute_pace(state);` call added in Task 2.
  - Remove the `predict` / `WindowSnapshot` imports.
  - Remove the `state.history` field from `CoordinatorState`, the `HISTORY_CAPACITY` const, and any `history`-init in the state constructor (it was only fed/used by the predictor).

- [ ] **Step 3: Remove the re-export + deps.**
  - `crates/state_coordinator/src/lib.rs`: delete `pub use predictor::{Prediction, PredictionState, WindowSnapshot};`.
  - `crates/state_coordinator/Cargo.toml`: delete the `predictor = { workspace = true }` line.
  - `Cargo.toml` (root): delete `predictor = { path = "crates/predictor" }` from `[workspace.dependencies]`.

- [ ] **Step 4: Remove `prediction` from `compose()`.** In `crates/snapshot_composer/src/lib.rs` delete the `prediction: None,` line (keep the `pace:` line from Task 2). Remove the now-stale comment that mentions "prediction" if present.

- [ ] **Step 5: Remove `JsonPrediction` from `--json`.** In `crates/balanze_cli/src/json_output.rs`: delete the `JsonPrediction` struct + its `impl From<&Prediction>`, the `prediction:` field in `JsonDoc`, the `prediction:` line in `from_snapshot`, and the `use ...predictor...` / `Prediction`/`PredictionState` imports. Update any json_output test that asserts on `.prediction` to assert on `.pace` instead (or drop the prediction assertion).

- [ ] **Step 6: Delete the crate directory.**

```bash
git rm -r crates/predictor
```

- [ ] **Step 7: Sweep for stragglers.**

Run: `rg -n "predictor|\bPrediction\b|\.prediction\b|WindowSnapshot|HISTORY_CAPACITY" crates/ src-tauri/ Cargo.toml`
Expected: no matches in code. Fix any that remain (e.g. a leftover test constructing `Snapshot { prediction: ... }`).

- [ ] **Step 8: Run gates.**

Run: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all PASS / clean.

- [ ] **Step 9: Commit.**

```bash
git add -A
git commit -m "refactor: retire predictor crate; pace is the forward-looking signal"
```

---

### Task 4: Reshape the CLI compact view (R1 matrix + pace line)

**Goal:** Make the compact view obey the matrix contract — API-$ column shows only real billed money (overage or "— not available"); the list-price estimate moves to a separate "Subscription leverage" line; add a pace line; rewrite the legend.

**Files:**
- Modify: `crates/balanze_cli/src/main.rs` (`write_compact`, `compact_anthropic_cost`; add `compact_subscription_leverage` + `compact_pace_line` helpers)
- Modify/add: the `write_compact` test(s) in `crates/balanze_cli/src/main.rs` (or wherever the compact-render tests live)

**Acceptance Criteria:**
- [ ] The Anthropic API-$ cell shows the `extra_usage` overage (real) when enabled, else `— not available`; it NEVER contains the list-price estimate.
- [ ] The list-price estimate renders on its own `Subscription leverage:` line below the matrix, labeled NOT billed.
- [ ] A `Pace:` line shows per-window `used% / elapsed% (ratio×)` when pace data is present; windows with `ratio: None` (just reset) show `used% / elapsed% (—)`.
- [ ] The legend no longer claims the estimate lives in the matrix.

**Verify:** `cargo test -p balanze_cli` → pass; manual `cargo run -p balanze_cli -- status` shows the reshaped layout.

**Steps:**

- [ ] **Step 1: Rewrite `compact_anthropic_cost`** in `crates/balanze_cli/src/main.rs` so the cell is real-only:

```rust
fn compact_anthropic_cost(s: &Snapshot) -> String {
    // Measured-only matrix cell: real billed money or nothing. The list-price
    // estimate is NOT here — it renders on the separate "Subscription leverage"
    // line (see compact_subscription_leverage).
    match s
        .claude_oauth
        .as_ref()
        .and_then(|o| o.extra_usage.as_ref())
        .filter(|eu| eu.is_enabled)
    {
        Some(eu) => format!(
            "{}/{} overage (real)",
            micro_usd_to_display_dollars(eu.used_credits_micro_usd),
            micro_usd_to_display_dollars(eu.monthly_limit_micro_usd)
        ),
        None => "— not available".to_string(),
    }
}
```

- [ ] **Step 2: Add the leverage + pace helpers** (near the other `compact_*` fns):

```rust
/// The JSONL list-price estimate, rendered as a clearly-secondary insight
/// OUTSIDE the matrix — what the local Claude Code usage would cost at API
/// list prices. Subscription leverage, never billed. `None` when there's no
/// JSONL data to estimate from.
fn compact_subscription_leverage(s: &Snapshot) -> Option<String> {
    match &s.anthropic_api_cost {
        Some(cost) if cost.total_event_count > 0 => Some(format!(
            "Subscription leverage: ~{} of Claude Code usage at API list prices (leverage — NOT billed)",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        )),
        _ => None,
    }
}

/// Per-window pace line: used % vs elapsed % of the window, plus the ratio.
/// `None` when no pace data is present.
fn compact_pace_line(s: &Snapshot) -> Option<String> {
    if s.pace.is_empty() {
        return None;
    }
    let parts: Vec<String> = s
        .pace
        .iter()
        .map(|p| {
            let ratio = match p.ratio {
                Some(r) => format!("{r:.1}×"),
                None => "—".to_string(),
            };
            format!(
                "{} {:.0}% used / {:.0}% elapsed ({ratio})",
                short_cadence(&p.key),
                p.used_fraction * 100.0,
                p.elapsed_fraction * 100.0,
            )
        })
        .collect();
    Some(format!("Pace: {}", parts.join(";  ")))
}
```

- [ ] **Step 3: Rewrite the tail of `write_compact`** (the header column label, the legend, and the new lines). Replace the column header line and everything from the legend block onward:

```rust
    writeln!(w, "                    {:38}  API $ (real billed)", "Quota %")?;
    writeln!(w, "Anthropic           {anth_quota:38}  {anth_cost}")?;
    writeln!(w, "OpenAI              {openai_quota:38}  {openai_cost}")?;
    writeln!(w)?;

    if let Some(pace) = compact_pace_line(snapshot) {
        writeln!(w, "{pace}")?;
    }
    if let Some(lev) = compact_subscription_leverage(snapshot) {
        writeln!(w, "{lev}")?;
    }
    writeln!(w)?;

    // The matrix holds measured reality only — server-reported quota % and
    // real billed $. The list-price estimate is the separate "Subscription
    // leverage" line above, never a matrix cell, so a ~$4,000 estimate is
    // never mistaken for ~$4,000 of real spend.
    writeln!(
        w,
        "Quota % = live server-reported utilization. API $ = real billed spend"
    )?;
    writeln!(
        w,
        "only: Anthropic = pay-as-you-go overage (n/a unless enabled); OpenAI ="
    )?;
    writeln!(w, "Admin Costs API. 'Subscription leverage' is a separate")?;
    writeln!(w, "list-price estimate, never charged.")?;
    writeln!(w)?;
    writeln!(
        w,
        "Run `balanze-cli --sections` for per-source detail, or `balanze-cli --json` for machine-readable output."
    )
```

- [ ] **Step 4: Update/add the compact-render test.** Find the existing `write_compact` test (search `write_compact` in `crates/balanze_cli/src/main.rs` test module). Update assertions: the rendered output must (a) contain `— not available` OR `overage (real)` in the Anthropic cost cell, (b) NOT contain `est-leverage` in the matrix rows, (c) contain `Subscription leverage:` when JSONL cost is present, (d) contain `Pace:` when `snapshot.pace` is non-empty. If a fixture `Snapshot` is built inline, set `pace` via `pace_for_oauth` or a literal `vec![WindowPace { key: "five_hour".into(), used_fraction: 0.82, elapsed_fraction: 0.40, ratio: Some(2.05) }]`. Example assertion block:

```rust
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("API $ (real billed)"));
    assert!(!out.contains("est-leverage"));
    assert!(out.contains("Subscription leverage:"));
    assert!(out.contains("Pace:"));
```

- [ ] **Step 5: Run gates + eyeball.**

Run: `cargo test -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo run -p balanze_cli -- status`
Expected: tests pass; the live run shows the reshaped layout (matrix real-only, leverage + pace lines below).

- [ ] **Step 6: Commit.**

```bash
git add crates/balanze_cli/src/main.rs
git commit -m "feat(cli): R1 compact view — measured-only matrix, separate leverage + pace lines"
```

---

### Task 5: Reference-doc lockstep

**Goal:** Bring the precise current-state docs in line with the code: crate map, boundaries, IPC `.pace`, validation matrix, the deleted troubleshooting entry, README `--json` + compact example (drop the "planned R1" caveat — R1 is now real), and the CHANGELOG.

**Files:**
- Modify: `docs/ARCHITECTURE.md` (crate map line ~47; boundary #2 line ~65; IPC `--json` `.prediction` → `.pace` line ~91)
- Modify: `AGENTS.md` (§0 calibration line ~13 drop "the predictor algorithm"; logging table line ~80 "predictor result computed" → "pace recomputed"; validation matrix line ~149 pure-crates list drop `predictor`; §7 line ~170 drop `predictor`; delete the §10 troubleshooting entry "Predictor returns confidently-wrong numbers right after window reset")
- Modify: `README.md` (the `--json` paragraph `.prediction` → `.pace`; remove the "planned R1 / current CLI differs" caveats added in PR #53; the compact example now matches real output)
- Modify: `CHANGELOG.md` (Unreleased: remove the predictor bullet, adjust the "two cells for Track E" bullet to drop `prediction`, add a pace + R1 entry)

**Acceptance Criteria:**
- [ ] No reference doc mentions the `predictor` crate, `Prediction`, or the EWMA warm-up as a current component (history/changelog narration of *why it was retired* is fine).
- [ ] `ARCHITECTURE.md` crate map omits `predictor`; boundary #2 lists only `window`, `claude_cost` as the pure crates; the IPC line documents `.pace` not `.prediction`.
- [ ] `AGENTS.md` §10 no longer contains the predictor troubleshooting entry.
- [ ] `README.md` no longer says the matrix layout is "planned" — it's current; `--json` doc references `.pace`.

**Verify:** `rg -n "predictor|Prediction|EWMA|predictive countdown|planned.*R1|being reshaped" README.md AGENTS.md docs/ARCHITECTURE.md` → only intentional historical mentions remain (e.g. CHANGELOG "retired the predictor"). Cross-check internal links resolve.

**Steps:**

- [ ] **Step 1: `docs/ARCHITECTURE.md`.** Crate map: delete the `predictor/` line; on the `window/` line note it also computes pace. Boundary #2: change "`window`, `predictor`, `claude_cost` are pure functions" → "`window`, `claude_cost` are pure functions". IPC contract: change the `.prediction` sentence to describe `.pace` (per-window used/elapsed/ratio).

- [ ] **Step 2: `AGENTS.md`.** §0 calibration list: remove "the predictor algorithm". Logging table: "predictor result computed" → "pace recomputed". Validation matrix "pure crates (`window`, `claude_cost`, `predictor`)" → "(`window`, `claude_cost`)". §7 "Especially `window`, `predictor`, `claude_parser`" → "Especially `window`, `claude_parser`". §10: delete the whole "Predictor returns confidently-wrong numbers right after window reset" troubleshooting subsection.

- [ ] **Step 3: `README.md`.** In the `--json` paragraph, replace the `.prediction` sentence with `.pace` (an array of per-window `{ key, used_fraction, elapsed_fraction, ratio }`). Remove the "**Note:** this measured-only matrix is the *planned* layout (R1)… current shipped CLI still prints the estimate in-cell" callout in the matrix section and the "**target layout (R1)** … current CLI still prints…" caveat on the example — R1 is now shipped, so describe it as current. Verify the example block matches the new compact output (real-only matrix + `Subscription leverage:` + `Pace:` lines).

- [ ] **Step 4: `CHANGELOG.md` (Unreleased).** Remove the `predictor` crate bullet. In the "Snapshot gained two cells" bullet, drop the `prediction` half. Add a new Unreleased bullet: "Replaced the EWMA predictor with a measured **pace view** (`Snapshot.pace`, `--json .pace`): per-window used % vs elapsed % + ratio, computed by the pure `window::pace`. The `predictor` crate is retired. Compact view reshaped to the measured-only matrix (R1): the list-price estimate moved out of the Anthropic API-$ cell to a separate 'Subscription leverage' line."

- [ ] **Step 5: Verify + commit.**

Run: `rg -n "predictor|Prediction|EWMA" README.md AGENTS.md docs/ARCHITECTURE.md`
Expected: no current-component mentions remain.

```bash
git add README.md AGENTS.md docs/ARCHITECTURE.md CHANGELOG.md
git commit -m "docs: lockstep ARCHITECTURE/AGENTS/README/CHANGELOG with pace + R1 (predictor retired)"
```

---

## Wrap-up

After Task 5, open a PR from the working branch to `main` titled (squash target):

`feat: align CLI + Snapshot with the merged model — R1 matrix, pace view, retire predictor`

PR body should note the §8 `Snapshot` schema change (`prediction` → `pace`) and that `--json` consumers should read `.pace` instead of `.prediction`. Run the full gate set (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`) before pushing.
