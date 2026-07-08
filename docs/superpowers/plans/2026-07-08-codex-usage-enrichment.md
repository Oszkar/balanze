# Codex Usage Enrichment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enrich the Codex column to show both rate-limit windows (5-hour + weekly) mirroring the Claude cell, with worst-window tray semantics, and parse token/context/credits as an internal (deferred) representation.

**Architecture:** The frontend already receives both `codex_quota.primary` and `codex_quota.secondary`; the user-facing change is presentation logic over already-serialized fields. Windows are classified by `window_duration_minutes` (300 = 5h, 10080 = weekly), never by slot position, because which slot holds which varies by plan/CLI version. Token/context/credits are parsed into `#[serde(skip)]` fields so the serialized `Snapshot` shape is byte-identical and `SNAPSHOT_SCHEMA_VERSION` stays at 3.

**Tech Stack:** Rust (workspace crates `codex_local`, `state_coordinator`, `balanze_cli`, `src-tauri`), Svelte 5 runes + TypeScript (SvelteKit), vitest, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-07-08-codex-usage-enrichment-design.md`

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/codex_local/src/types.rs` | `WindowKind`, window accessors, internal `CodexTokenUsage`/`CodexCredits` types + `#[serde(skip)]` fields | 1 |
| `crates/codex_local/src/parser.rs` | Parse `info` token/context, compute burn, parse credits | 1 |
| `crates/codex_local/src/lib.rs` | Re-export new types | 1 |
| `crates/codex_local/SCHEMA-NOTES.md` | Document window layout + burn limitation + backend-only fields | 1 |
| (7 test constructors across crates) | Add `tokens: None, credits: None` to compile | 1 |
| `src/lib/presentation/quota.ts` | `codexWindowsByKind()` + `codexQuota()` builders | 2 |
| `src/lib/presentation/quota.test.ts` | Unit tests for the builders | 2 |
| `src/lib/components/GridView.svelte` | 5h headline + weekly secondary in the Codex cell | 3 |
| `src/lib/components/CardsView.svelte` | 5h + weekly cards | 3 |
| `src-tauri/src/tauri_sink.rs` | Tray Codex 5h/weekly split, worst-window | 4 |
| `crates/balanze_cli/src/render.rs` | Label windows 5h/weekly by kind | 5 |
| `crates/balanze_cli/src/statusline.rs` | Codex statusline % = worst window | 5 |
| `crates/balanze_cli/src/tui.rs` | TUI gauge = worst window | 5 |
| `README.md`, `docs/PRD.md` | Doc lockstep note | 6 |

---

### Task 1: codex_local internal data enrichment

**Goal:** `codex_local` classifies windows by duration, exposes 5h/weekly/worst accessors, and parses token/context/credits into `#[serde(skip)]` internal fields (parsed + tested, never serialized).

**Files:**
- Modify: `crates/codex_local/src/types.rs`
- Modify: `crates/codex_local/src/parser.rs`
- Modify: `crates/codex_local/src/lib.rs`
- Modify: `crates/codex_local/SCHEMA-NOTES.md`
- Modify (add `tokens: None, credits: None` to each literal `CodexQuotaSnapshot { ... }`):
  - `src-tauri/src/tauri_sink.rs:577`
  - `crates/balanze_cli/src/json_output.rs:480`
  - `crates/balanze_cli/src/sinks.rs:283`
  - `crates/balanze_cli/src/tui.rs:708`
  - `crates/balanze_cli/src/statusline.rs:323` and `:362`
  - `crates/balanze_cli/src/render.rs:857` and `:1489`
  - `crates/state_coordinator/src/snapshot_file.rs:211`

**Acceptance Criteria:**
- [ ] `RateLimitWindow::kind()` returns `FiveHour` for 300, `Weekly` for 10080, `Other` otherwise.
- [ ] `CodexQuotaSnapshot::five_hour()`/`weekly()`/`worst_window()` select by duration/utilization regardless of slot.
- [ ] `tokens`/`credits` are populated by the parser but absent from `serde_json::to_string`.
- [ ] `recent_burn_tokens_per_min` is `Some` only with >= 2 monotonic token samples; `None` for 1 event or a negative delta.
- [ ] `credits.balance` parses the JSON string `"0"` to `Some(0)`; `null` -> `None`; non-numeric -> `None`.
- [ ] `cargo nextest run -p codex_local` and `cargo build --workspace` are green.

**Verify:** `cargo nextest run -p codex_local` -> all pass; `cargo build --workspace` -> compiles.

**Steps:**

- [ ] **Step 1: Add `WindowKind`, accessors, and internal types to `types.rs`.**

Add near the top of `crates/codex_local/src/types.rs` (after the imports), and fix the inverted doc comments on `RateLimitWindow`/`CodexQuotaSnapshot` (delete "7-day rolling"/"primary is always present (7-day)" claims; replace with "classified by duration, not slot"):

```rust
/// Which rolling window a [`RateLimitWindow`] represents, classified by its
/// duration. Codex reports two lengths in practice: 300 minutes (5 hours)
/// and 10080 minutes (7 days / weekly). Which JSON slot (`primary`/`secondary`)
/// holds which VARIES by plan and CLI version: on "go" a single weekly window
/// sits in `primary`; on "plus"/"pro" `primary` is the 5-hour window and
/// `secondary` is weekly. Consumers MUST classify by duration, never by slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    FiveHour,
    Weekly,
    Other,
}

/// Per-session token/context accounting from the `token_count` event's `info`
/// block. INTERNAL ONLY: `#[serde(skip)]` on the snapshot keeps these off the
/// IPC wire. Parsed and tested now; surfacing them in any UI is deferred
/// (see SCHEMA-NOTES.md "Token/context: internal only").
///
/// LIMITATION: Codex's cap is percentage-windows, not tokens, so token burn
/// does NOT predict quota exhaustion (unlike Claude, whose cap IS tokens). The
/// eventual actionable metric is context-window fill
/// (`last_input_tokens` / `context_window`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CodexTokenUsage {
    pub context_window: u64,
    pub last_input_tokens: u64,
    pub last_total_tokens: u64,
    pub session_total_tokens: u64,
    /// Tokens/min between the last two `token_count` events in the session.
    /// `None` with fewer than two events or a non-monotonic counter.
    pub recent_burn_tokens_per_min: Option<f64>,
}

/// Codex credits balance. INTERNAL ONLY (see [`CodexTokenUsage`]). For observed
/// data this is effectively always zero/absent; the real balance and per-model
/// (Spark) caps are backend-only and NOT obtainable from local files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CodexCredits {
    pub has_credits: bool,
    pub balance: Option<i64>,
}

impl RateLimitWindow {
    /// Classify this window by its duration. See [`WindowKind`].
    pub fn kind(&self) -> WindowKind {
        match self.window_duration_minutes {
            300 => WindowKind::FiveHour,
            10080 => WindowKind::Weekly,
            _ => WindowKind::Other,
        }
    }
}

impl CodexQuotaSnapshot {
    /// All present windows: `primary` always, `secondary` if any.
    pub fn windows(&self) -> impl Iterator<Item = &RateLimitWindow> {
        std::iter::once(&self.primary).chain(self.secondary.iter())
    }
    /// The 5-hour window, if present in either slot. `None` on plans that only
    /// expose a weekly window (e.g. "go").
    pub fn five_hour(&self) -> Option<&RateLimitWindow> {
        self.windows().find(|w| w.kind() == WindowKind::FiveHour)
    }
    /// The weekly (7-day) window, if present in either slot.
    pub fn weekly(&self) -> Option<&RateLimitWindow> {
        self.windows().find(|w| w.kind() == WindowKind::Weekly)
    }
    /// The highest-utilization window ("how close to a limit am I"). Always
    /// `Some` because `primary` is always present.
    pub fn worst_window(&self) -> Option<&RateLimitWindow> {
        self.windows()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    }
}
```

Add the two internal fields to the `CodexQuotaSnapshot` struct (after `rate_limit_reached`):

```rust
    /// INTERNAL, not serialized (`#[serde(skip)]`). Token/context accounting;
    /// deferred from UI. See [`CodexTokenUsage`].
    #[serde(skip)]
    pub tokens: Option<CodexTokenUsage>,
    /// INTERNAL, not serialized. Credits balance; deferred from UI.
    #[serde(skip)]
    pub credits: Option<CodexCredits>,
```

- [ ] **Step 2: Write failing accessor + serialization tests in `types.rs`.**

Add to the `#[cfg(test)] mod tests` in `types.rs` (create the module if absent; import `chrono::TimeZone`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn win(pct: f64, mins: u64) -> RateLimitWindow {
        RateLimitWindow { used_percent: pct, window_duration_minutes: mins, resets_at: Utc.timestamp_opt(2000, 0).unwrap() }
    }
    fn snap(primary: RateLimitWindow, secondary: Option<RateLimitWindow>) -> CodexQuotaSnapshot {
        CodexQuotaSnapshot {
            observed_at: Utc.timestamp_opt(1000, 0).unwrap(),
            session_id: "s".into(),
            primary, secondary,
            plan_type: "pro".into(),
            rate_limit_reached: false,
            tokens: None, credits: None,
        }
    }

    #[test]
    fn classifies_and_selects_windows_by_duration_not_slot() {
        // plus/pro order: primary=5h, secondary=weekly.
        let s = snap(win(1.0, 300), Some(win(2.0, 10080)));
        assert_eq!(s.five_hour().unwrap().used_percent, 1.0);
        assert_eq!(s.weekly().unwrap().used_percent, 2.0);
        assert_eq!(s.worst_window().unwrap().used_percent, 2.0);
        // go order: single weekly window in primary.
        let g = snap(win(3.0, 10080), None);
        assert!(g.five_hour().is_none());
        assert_eq!(g.weekly().unwrap().used_percent, 3.0);
        assert_eq!(g.worst_window().unwrap().used_percent, 3.0);
    }

    #[test]
    fn internal_fields_are_not_serialized() {
        let mut s = snap(win(1.0, 300), Some(win(2.0, 10080)));
        s.tokens = Some(CodexTokenUsage { context_window: 258400, session_total_tokens: 999, ..Default::default() });
        s.credits = Some(CodexCredits { has_credits: false, balance: Some(0) });
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("tokens"), "{json}");
        assert!(!json.contains("credits"), "{json}");
        assert!(!json.contains("context_window"), "{json}");
        // round-trips back to None (skipped fields default on deserialize).
        let back: CodexQuotaSnapshot = serde_json::from_str(&json).unwrap();
        assert!(back.tokens.is_none() && back.credits.is_none());
    }
}
```

Run: `cargo nextest run -p codex_local classifies_and_selects` -> FAIL (accessors/fields not yet compiled if Step 1 incomplete) or PASS once Step 1 lands. The serialization test locks the no-schema-change invariant.

- [ ] **Step 3: Parse token/context/credits + burn in `parser.rs`.**

In `crates/codex_local/src/parser.rs`, extend the `use crate::types::{...}` (line 39) to:

```rust
use crate::types::{CodexCredits, CodexQuotaSnapshot, CodexTokenUsage, RateLimitWindow};
```

Add a burn accumulator before the loop (after `let mut last_drift_line: usize = 0;`, line 64):

```rust
    // Previous (observed_at, cumulative session tokens) for burn between the
    // last two token_count events. Only advances on events that carry tokens.
    let mut prev_token_sample: Option<(DateTime<Utc>, u64)> = None;
```

Replace the `latest = Some(CodexQuotaSnapshot { ... });` block (currently lines 158-165) with token/credits parsing followed by the struct build:

```rust
        let mut tokens = parse_token_usage(payload.pointer("/info"));
        if let Some(t) = tokens.as_mut() {
            t.recent_burn_tokens_per_min = match prev_token_sample {
                Some((prev_ts, prev_total))
                    if observed_at > prev_ts && t.session_total_tokens >= prev_total =>
                {
                    let mins = (observed_at - prev_ts).num_seconds() as f64 / 60.0;
                    (mins > 0.0).then(|| (t.session_total_tokens - prev_total) as f64 / mins)
                }
                _ => None,
            };
            prev_token_sample = Some((observed_at, t.session_total_tokens));
        }
        let credits = parse_credits(rate_limits.get("credits"));

        latest = Some(CodexQuotaSnapshot {
            observed_at,
            session_id: session_id.clone(),
            primary,
            secondary,
            plan_type,
            rate_limit_reached,
            tokens,
            credits,
        });
```

Add the two helper functions next to `parse_window` (after it, around line 197):

```rust
/// Parse the `info` block into internal token/context accounting. Returns
/// `None` when no token counts are present (nothing useful to record).
fn parse_token_usage(info: Option<&Value>) -> Option<CodexTokenUsage> {
    let info = info?.as_object()?;
    let context_window = info.get("model_context_window").and_then(|v| v.as_u64()).unwrap_or(0);
    let last_input_tokens = info.pointer("/last_token_usage/input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let last_total_tokens = info.pointer("/last_token_usage/total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let session_total_tokens = info.pointer("/total_token_usage/total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    if context_window == 0 && session_total_tokens == 0 && last_total_tokens == 0 {
        return None;
    }
    Some(CodexTokenUsage {
        context_window,
        last_input_tokens,
        last_total_tokens,
        session_total_tokens,
        recent_burn_tokens_per_min: None,
    })
}

/// Parse the `credits` object. `null` -> `None`. `balance` is a JSON string
/// (e.g. "0") parsed to `i64` when numeric.
fn parse_credits(credits: Option<&Value>) -> Option<CodexCredits> {
    let obj = credits?.as_object()?;
    let has_credits = obj.get("has_credits").and_then(|v| v.as_bool()).unwrap_or(false);
    let balance = obj.get("balance").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok());
    Some(CodexCredits { has_credits, balance })
}
```

- [ ] **Step 4: Write parser tests for tokens/burn/credits.**

Add to `parser.rs` `#[cfg(test)] mod tests` (reuse its `write_temp`/`NamedTempFile` helpers; the constants below are self-contained):

```rust
    #[test]
    fn parses_tokens_and_recent_burn_across_two_events() {
        // Two token_count events 2 minutes apart: session tokens 1000 -> 4000.
        const E1: &str = r#"{"timestamp":"2026-07-08T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1000},"last_token_usage":{"input_tokens":900,"total_tokens":1000},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":1.0,"window_minutes":300,"resets_at":1783490621},"secondary":{"used_percent":2.0,"window_minutes":10080,"resets_at":1784003136},"plan_type":"pro","rate_limit_reached_type":null}}}"#;
        const E2: &str = r#"{"timestamp":"2026-07-08T10:02:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":4000},"last_token_usage":{"input_tokens":1200,"total_tokens":1500},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":1.0,"window_minutes":300,"resets_at":1783490621},"secondary":{"used_percent":2.0,"window_minutes":10080,"resets_at":1784003136},"plan_type":"pro","rate_limit_reached_type":null}}}"#;
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{SESSION_META}\n{E1}\n{E2}\n").unwrap();
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        let t = snap.tokens.expect("tokens parsed");
        assert_eq!(t.context_window, 258400);
        assert_eq!(t.session_total_tokens, 4000);
        assert_eq!(t.last_input_tokens, 1200);
        // (4000 - 1000) tokens over 2.0 minutes = 1500 tok/min.
        assert_eq!(t.recent_burn_tokens_per_min, Some(1500.0));
    }

    #[test]
    fn single_event_has_no_burn() {
        const E1: &str = r#"{"timestamp":"2026-07-08T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1000},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":1.0,"window_minutes":300,"resets_at":1783490621},"plan_type":"pro","rate_limit_reached_type":null}}}"#;
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{SESSION_META}\n{E1}\n").unwrap();
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        assert_eq!(snap.tokens.unwrap().recent_burn_tokens_per_min, None);
    }

    #[test]
    fn credits_string_balance_parsed() {
        const E1: &str = r#"{"timestamp":"2026-07-08T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":{"primary":{"used_percent":1.0,"window_minutes":300,"resets_at":1783490621},"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"plan_type":"plus","rate_limit_reached_type":null}}}"#;
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{SESSION_META}\n{E1}\n").unwrap();
        let snap = read_latest_quota_snapshot(f.path()).unwrap().unwrap();
        let c = snap.credits.expect("credits parsed");
        assert_eq!(c.balance, Some(0));
        assert!(!c.has_credits);
        // info was empty -> no token usage recorded.
        assert!(snap.tokens.is_none());
    }
```

Note: if the existing test module writes fixtures differently (it uses `NamedTempFile` per parser.rs tests), match that helper. Run: `cargo nextest run -p codex_local` -> all pass.

- [ ] **Step 5: Re-export new types + update the 7 downstream literal constructors.**

In `crates/codex_local/src/lib.rs:60`, extend the re-export:

```rust
pub use types::{CodexCredits, CodexQuotaSnapshot, CodexTokenUsage, RateLimitWindow, WindowKind};
```

In each of these files, add `tokens: None,` and `credits: None,` as the last two fields of the literal `CodexQuotaSnapshot { ... }` (they are all test/sample constructors):
`src-tauri/src/tauri_sink.rs:577`, `crates/balanze_cli/src/json_output.rs:480`, `crates/balanze_cli/src/sinks.rs:283`, `crates/balanze_cli/src/tui.rs:708`, `crates/balanze_cli/src/statusline.rs:323` and `:362`, `crates/balanze_cli/src/render.rs:857` and `:1489`, `crates/state_coordinator/src/snapshot_file.rs:211`.

Run: `cargo build --workspace` -> compiles.

- [ ] **Step 6: Document in SCHEMA-NOTES.md.**

Append a section to `crates/codex_local/SCHEMA-NOTES.md`:

```markdown
## 2026-07-08 update: two windows, internal token/context, backend-only fields

- **Windows vary by plan.** "go" reports ONE window (weekly, `window_minutes: 10080`) in `primary`, `secondary: null`. "plus"/"pro" report TWO: `primary` = 5-hour (300), `secondary` = weekly (10080). Confirmed across 447 files spanning CLI 0.130.0 -> 0.143.0. Classify by `window_minutes` (`RateLimitWindow::kind()`), never by slot. The old "primary is 7-day" comment was only ever true on "go".
- **Token/context: internal only.** `info.total_token_usage`, `info.last_token_usage`, `info.model_context_window` are parsed into `CodexTokenUsage` (`#[serde(skip)]`), tested, but NOT surfaced in any UI yet. LIMITATION: Codex's cap is percentage-windows, not tokens, so token burn does NOT predict quota exhaustion (unlike Claude). The eventual actionable metric is context-window fill. Exposure and presentation deferred.
- **Backend-only fields.** `individual_limit` (per-model GPT-5.3-Codex-Spark caps) is `null` in 0 of 447 files, including sessions that invoked Spark; it is backend/web-only. `credits.balance` is only ever `"0"`/null for observed plans. Neither the Spark caps nor a real credits balance is obtainable from local files; that would require the ChatGPT backend API + Codex OAuth (out of scope).
```

- [ ] **Step 7: Validate and commit.**

Run: `cargo nextest run -p codex_local && cargo build --workspace && cargo clippy -p codex_local --all-targets -- -D warnings && cargo fmt --all -- --check`

```bash
git add crates/codex_local src-tauri/src/tauri_sink.rs crates/balanze_cli/src crates/state_coordinator/src/snapshot_file.rs
git commit -m "feat(codex_local): classify windows by duration and parse token/context/credits internally"
```

---

### Task 2: Frontend codexQuota builders

**Goal:** Add `codexWindowsByKind()` and `codexQuota()` to `quota.ts`, mirroring `anthropicQuota()`, classifying windows by duration.

**Files:**
- Modify: `src/lib/presentation/quota.ts`
- Modify: `src/lib/presentation/quota.test.ts`

**Acceptance Criteria:**
- [ ] `codexQuota()` returns a 5h headline + weekly `secondaryPct` on plus/pro (both windows).
- [ ] On go (single weekly window) the headline is the weekly window and `secondaryPct` is `null`.
- [ ] `null` snapshot -> `codexQuota()` returns `null`.
- [ ] `bunx vitest run src/lib/presentation/quota.test.ts` passes.

**Verify:** `bunx vitest run src/lib/presentation/quota.test.ts` -> pass.

**Steps:**

- [ ] **Step 1: Write failing tests in `quota.test.ts`.**

Add `codexQuota` to the import on line 2, then append inside `describe('quota', ...)`:

```ts
  const codexSnap = (primary: { used_percent: number; window_duration_minutes: number; resets_at: string }, secondary: typeof primary | null, plan = 'pro'): Snapshot => ({
    ...base,
    codex_quota: { observed_at: '2026-07-08T10:00:00Z', session_id: 's', primary, secondary, plan_type: plan, rate_limit_reached: false },
  });

  it('codexQuota: 5h headline + weekly secondary on two-window plans', () => {
    const s = codexSnap(
      { used_percent: 1, window_duration_minutes: 300, resets_at: '2026-07-08T06:03:41Z' },
      { used_percent: 2, window_duration_minutes: 10080, resets_at: '2026-07-14T04:25:36Z' },
    );
    const q = codexQuota(s)!;
    expect(q.headline.label).toBe('5h');
    expect(q.headline.pct).toBe(1);
    expect(q.secondaryPct).toBe(2);
    expect(q.plan).toBe('pro');
  });

  it('codexQuota: single weekly window becomes the headline (go plan)', () => {
    const s = codexSnap({ used_percent: 3, window_duration_minutes: 10080, resets_at: '2026-07-14T04:25:36Z' }, null, 'go');
    const q = codexQuota(s)!;
    expect(q.headline.label).toBe('weekly');
    expect(q.headline.pct).toBe(3);
    expect(q.secondaryPct).toBeNull();
  });

  it('codexQuota: null snapshot -> null', () => {
    expect(codexQuota(base)).toBeNull();
  });
```

Run: `bunx vitest run src/lib/presentation/quota.test.ts` -> FAIL (`codexQuota` not exported).

- [ ] **Step 2: Implement the builders in `quota.ts`.**

Add imports for the Codex types at the top of `quota.ts` (alongside the existing `Snapshot` import):

```ts
import type { CodexQuotaSnapshot, RateLimitWindow } from '$lib/types/snapshot';
```

Append (near `codexElapsedFraction`), reusing the existing `quotaTone` + `Tone`:

```ts
export interface CodexQuota {
  headline: { pct: number; resetsAt: string; window: RateLimitWindow; label: '5h' | 'weekly' | 'codex' };
  secondaryPct: number | null;
  plan: string;
  tone: Tone;
}

// Codex reports windows of 300 min (5h) and 10080 min (weekly); which JSON slot
// holds which varies by plan, so select by duration, never by position.
export function codexWindowsByKind(q: CodexQuotaSnapshot): { five: RateLimitWindow | null; weekly: RateLimitWindow | null } {
  const windows = [q.primary, ...(q.secondary ? [q.secondary] : [])];
  return {
    five: windows.find((w) => w.window_duration_minutes === 300) ?? null,
    weekly: windows.find((w) => w.window_duration_minutes === 10080) ?? null,
  };
}

export function codexQuota(s: Snapshot): CodexQuota | null {
  const q = s.codex_quota;
  if (!q) return null;
  const { five, weekly } = codexWindowsByKind(q);
  const headlineWin = five ?? weekly ?? q.primary;
  const label = headlineWin === five ? '5h' : headlineWin === weekly ? 'weekly' : 'codex';
  return {
    headline: { pct: headlineWin.used_percent, resetsAt: headlineWin.resets_at, window: headlineWin, label },
    secondaryPct: five && weekly ? weekly.used_percent : null,
    plan: q.plan_type,
    tone: quotaTone(headlineWin.used_percent),
  };
}
```

Run: `bunx vitest run src/lib/presentation/quota.test.ts` -> PASS.

- [ ] **Step 3: Commit.**

```bash
git add src/lib/presentation/quota.ts src/lib/presentation/quota.test.ts
git commit -m "feat(ui): add codexQuota window-classification builders"
```

---

### Task 3: Render weekly window in GridView + CardsView

**Goal:** The popover (GridView) shows a 5h headline + `weekly X% . plan` secondary; the cards view (CardsView) shows one card per present Codex window.

**Files:**
- Modify: `src/lib/components/GridView.svelte:3,20,90-94`
- Modify: `src/lib/components/CardsView.svelte:3,110-115`

**Acceptance Criteria:**
- [ ] GridView Codex cell headline is the 5h window; the meta row shows `weekly X% . <plan>` (or just `<plan>` on go).
- [ ] CardsView shows two Codex cards (5h + weekly) on plus/pro, one on go.
- [ ] `bun run check` passes; cell renders in `bun run tauri dev`.

**Verify:** `bun run check` -> clean; visual check in `bun run tauri dev` (Codex cell shows 5h + weekly).

**Steps:**

- [ ] **Step 1: GridView - import + derived.**

In `src/lib/components/GridView.svelte`, add `codexQuota` to the import on line 3:

```svelte
  import { anthropicQuota, quotaTone, codexElapsedFraction, codexWindowExpired, codexQuota } from '$lib/presentation/quota';
```

After line 20 (`const codex = $derived(snapshot.codex_quota);`) add:

```svelte
  const cq = $derived(codexQuota(snapshot));
```

- [ ] **Step 2: GridView - replace the Codex `QuotaCell` (lines 90-94).**

Replace the `{:else if codex}` block with:

```svelte
    {:else if cq}
      <QuotaCell pct={cq.headline.pct} used={cq.headline.pct}
        elapsed={codexElapsedFraction(cq.headline.window, snapshot.fetched_at) * 100} tone={cq.tone}
        resetsAt={cq.headline.resetsAt}
        secondary={cq.secondaryPct !== null ? `weekly ${cq.secondaryPct.toFixed(0)}% · ${cq.plan}` : cq.plan}
        stale={!!degraded['codex_quota'] || codexWindowExpired(cq.headline.window, snapshot.fetched_at)} staleLabel="stale" title={PROV.codexQuota.title} />
```

(`codex` stays as-is on line 20/36 for `colState`'s `hasData` check.)

- [ ] **Step 3: CardsView - import + windows.**

In `src/lib/components/CardsView.svelte`, add `codexWindowsByKind` to the import on line 3:

```svelte
  import { anthropicQuota, quotaTone, codexElapsedFraction, codexWindowExpired, codexWindowsByKind } from '$lib/presentation/quota';
```

Replace the `codexWindows` derived (lines 110-115) with:

```svelte
  const codexWindows = $derived.by<CardWindow[]>(() => {
    if (!codex) return [];
    const { five, weekly } = codexWindowsByKind(codex);
    const out: CardWindow[] = [];
    for (const [win, name] of [[five, '5h'], [weekly, 'weekly']] as const) {
      if (!win) continue;
      out.push({
        label: `Codex ${name} · ${codex.plan_type}`,
        used: win.used_percent,
        elapsed: codexElapsedFraction(win, snapshot.fetched_at) * 100,
        tone: quotaTone(win.used_percent),
        resetsAt: win.resets_at,
        stale: codexWindowExpired(win, snapshot.fetched_at) || !!degraded['codex_quota'],
        title: PROV.codexQuota.title,
      });
    }
    return out;
  });
```

Run: `bun run check` -> clean.

- [ ] **Step 4: Commit.**

```bash
git add src/lib/components/GridView.svelte src/lib/components/CardsView.svelte
git commit -m "feat(ui): show Codex 5h + weekly windows in popover and cards"
```

---

### Task 4: Tray Codex 5h/weekly split

**Goal:** `TrayView` splits Codex into `codex_5h`/`codex_weekly`; the title shows the worst Codex window; the tooltip shows a `5h X%  wk Y%` split.

**Files:**
- Modify: `src-tauri/src/tauri_sink.rs` (`TrayView` struct ~114, `from_snapshot` ~154-157, `has_data` ~163, `worst` ~180-189, `tray_title` ~220, `tray_tooltip` ~250-252, test helper ~575 + tests)

**Acceptance Criteria:**
- [ ] `TrayView` has `codex_5h` and `codex_weekly`; windows fold in by duration (300 -> 5h, else weekly).
- [ ] `tray_title` Codex figure = worst of the two; `tray_tooltip` Codex line shows both when present.
- [ ] `worst()` can name a Codex window (`Codex 5h` / `Codex wk`).
- [ ] `cargo nextest run -p balanze` (or the `src-tauri` package name) passes; existing tray tests stay green.

**Verify:** `cargo nextest run -p balanze` (src-tauri crate) -> pass; `bun run tauri dev` -> tray title/tooltip show Codex windows.

**Steps:**

- [ ] **Step 1: Replace the `TrayView` struct (lines 114-119).**

```rust
struct TrayView {
    claude_5h: Option<f32>,
    claude_7d: Option<f32>,
    codex_5h: Option<f32>,
    codex_weekly: Option<f32>,
}
```

- [ ] **Step 2: Fold Codex windows by duration in `from_snapshot` (replace lines 154-157).**

```rust
        if let Some(q) = &s.codex_quota {
            for w in q.windows() {
                if w.window_duration_minutes == 300 {
                    fold_max(&mut v.codex_5h, w.used_percent as f32);
                } else {
                    // Weekly (10080) or any other duration folds into the weekly
                    // slot so no window can hide from the ring/title.
                    fold_max(&mut v.codex_weekly, w.used_percent as f32);
                }
            }
        }
```

- [ ] **Step 3: Update `has_data`, add `codex_worst`, extend `worst()`.**

`has_data` (lines 163-165):

```rust
    fn has_data(&self) -> bool {
        self.claude_5h.is_some() || self.claude_7d.is_some()
            || self.codex_5h.is_some() || self.codex_weekly.is_some()
    }
```

Add after `claude_worst` (after line 174):

```rust
    /// Codex's worst window (max of 5h and weekly) - the single Codex figure
    /// shown in the menu-bar title.
    fn codex_worst(&self) -> Option<f32> {
        match (self.codex_5h, self.codex_weekly) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
```

`worst()` array (lines 181-185):

```rust
        [
            ("Claude 5h", self.claude_5h),
            ("Claude 7d", self.claude_7d),
            ("Codex 5h", self.codex_5h),
            ("Codex wk", self.codex_weekly),
        ]
```

- [ ] **Step 4: `tray_title` + `tray_tooltip` Codex lines.**

`tray_title` (line 220):

```rust
    let o = view.codex_worst().map(|p| format!("Codex {}%", p.round() as i64));
```

`tray_tooltip` - replace the single Codex line (lines 250-252) with:

```rust
    let mut codex = Vec::new();
    if let Some(p) = view.codex_5h {
        codex.push(format!("5h {}%", p.round() as i64));
    }
    if let Some(p) = view.codex_weekly {
        codex.push(format!("wk {}%", p.round() as i64));
    }
    if !codex.is_empty() {
        lines.push(format!("Codex   {}", codex.join("  ")));
    }
```

- [ ] **Step 5: Update the test helper + add a split test.**

`codex_with_util` (line 577) - add the two new fields (it builds a weekly window, which now folds to `codex_weekly`; existing title/tooltip tests still pass):

```rust
        codex_local::CodexQuotaSnapshot {
            observed_at: Utc::now(),
            session_id: "test".into(),
            primary: codex_local::RateLimitWindow {
                used_percent: util,
                window_duration_minutes: 10080,
                resets_at: Utc::now(),
            },
            secondary: None,
            plan_type: "go".into(),
            rate_limit_reached: false,
            tokens: None,
            credits: None,
        }
```

Add a two-window helper + test:

```rust
    fn codex_5h_weekly(five: f64, weekly: f64) -> codex_local::CodexQuotaSnapshot {
        use chrono::Utc;
        codex_local::CodexQuotaSnapshot {
            observed_at: Utc::now(),
            session_id: "test".into(),
            primary: codex_local::RateLimitWindow { used_percent: five, window_duration_minutes: 300, resets_at: Utc::now() },
            secondary: Some(codex_local::RateLimitWindow { used_percent: weekly, window_duration_minutes: 10080, resets_at: Utc::now() }),
            plan_type: "pro".into(),
            rate_limit_reached: false,
            tokens: None,
            credits: None,
        }
    }

    #[test]
    fn codex_splits_into_5h_and_weekly() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        s.codex_quota = Some(codex_5h_weekly(12.0, 80.0));
        let view = TrayView::from_snapshot(&s);
        assert_eq!(view.codex_5h, Some(12.0));
        assert_eq!(view.codex_weekly, Some(80.0));
        assert_eq!(tray_title(&view), "Codex 80%");
        let tip = tray_tooltip(&view, false);
        assert!(tip.contains("worst: Codex wk 80%"), "{tip}");
        assert!(tip.contains("5h 12%"), "{tip}");
        assert!(tip.contains("wk 80%"), "{tip}");
    }
```

Run: `cargo nextest run -p balanze` (the src-tauri crate; confirm its package name via `cargo metadata` or the crate's `Cargo.toml`) -> pass. If clippy flags the crate: `cargo clippy -p balanze --all-targets -- -D warnings`.

- [ ] **Step 6: Commit.**

```bash
git add src-tauri/src/tauri_sink.rs
git commit -m "feat(tray): split Codex into 5h and weekly windows with worst-window title"
```

---

### Task 5: CLI, statusline, and TUI worst-window + labels

**Goal:** The full CLI render labels windows 5h/weekly by kind; the statusline and TUI show the worst Codex window instead of `primary`.

**Files:**
- Modify: `crates/balanze_cli/src/render.rs:327-349`
- Modify: `crates/balanze_cli/src/statusline.rs:198-201`
- Modify: `crates/balanze_cli/src/tui.rs:345`

**Acceptance Criteria:**
- [ ] CLI full render prints each present window labeled `5h` / `weekly` (or `window` for an unknown duration).
- [ ] `statusline` Codex % and the TUI gauge use `worst_window().used_percent`.
- [ ] `cargo nextest run -p balanze_cli` passes (including a new worst-window statusline test).

**Verify:** `cargo nextest run -p balanze_cli` -> pass; `cargo run -p balanze_cli -- --json` sanity check shows correct Codex windows.

**Steps:**

- [ ] **Step 1: CLI render - label windows by kind (replace lines 327-349).**

Replace the `Primary window` / `Secondary window` blocks with a loop over the present windows:

```rust
        for win in q.windows() {
            let label = match win.window_duration_minutes {
                300 => "5h",
                10080 => "weekly",
                _ => "window",
            };
            let resets_in = win.resets_at.signed_duration_since(snapshot.fetched_at);
            writeln!(
                w,
                "  {label:<8} window: {:.2}% of {} minutes  (resets in {})",
                win.used_percent,
                win.window_duration_minutes,
                pretty_duration(resets_in),
            )?;
        }
```

(The `q.rate_limit_reached` warning + `verbose` session-id lines that follow stay unchanged.)

- [ ] **Step 2: statusline producer - worst window (replace lines 198-201).**

```rust
        codex_used_percent: snap
            .codex_quota
            .as_ref()
            .and_then(|q| q.worst_window())
            .map(|w| w.used_percent as f32),
```

- [ ] **Step 3: TUI gauge - worst window (line 345).**

```rust
            frame.render_widget(
                quota_gauge("Codex", q.worst_window().map_or(0.0, |w| w.used_percent) as f32),
                rows[0],
            );
```

- [ ] **Step 4: Add a worst-window statusline test.**

Add to `statusline.rs` tests (mirrors `cross_from_payload_maps_cells_and_freshness` but with two windows where weekly > 5h):

```rust
    #[test]
    fn cross_uses_worst_codex_window() {
        use chrono::TimeZone as _;
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 8, 12, 0, 0).unwrap();
        let mut s = state_coordinator::Snapshot::empty(now);
        s.codex_quota = Some(codex_local::types::CodexQuotaSnapshot {
            observed_at: now,
            session_id: "s".into(),
            primary: codex_local::types::RateLimitWindow { used_percent: 1.0, window_duration_minutes: 300, resets_at: now },
            secondary: Some(codex_local::types::RateLimitWindow { used_percent: 6.0, window_duration_minutes: 10_080, resets_at: now }),
            plan_type: "pro".into(),
            rate_limit_reached: false,
            tokens: None,
            credits: None,
        });
        let payload = state_coordinator::SnapshotFilePayload::new(s, now);
        let c = super::cross_from_payload(&payload, now);
        // worst window is the weekly at 6%, not the 5h at 1%.
        assert_eq!(c.codex_used_percent, Some(6.0));
    }
```

Run: `cargo nextest run -p balanze_cli` -> pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/balanze_cli/src/render.rs crates/balanze_cli/src/statusline.rs crates/balanze_cli/src/tui.rs
git commit -m "feat(cli): label Codex windows by kind and use worst window in statusline/tui"
```

---

### Task 6: Docs lockstep + full validation + PR

**Goal:** Update user-facing docs for the richer Codex column, run the full validation matrix, and open the PR.

**Files:**
- Modify: `README.md` (Codex column description, if it names the single window)
- Modify: `docs/PRD.md` (only if it describes the Codex cell's window)

**Acceptance Criteria:**
- [ ] README/PRD mention the Codex column shows 5h + weekly windows (no stale "single window" wording).
- [ ] Full validation matrix green.
- [ ] PR opened against `main`.

**Verify:** `cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace && cargo fmt --all -- --check && bun run check && bunx vitest run` -> all green.

**Steps:**

- [ ] **Step 1: Update docs.**

Grep for stale single-window Codex wording and update it:

```bash
grep -rn -i "codex" README.md docs/PRD.md | grep -i -E "window|quota|weekly|7-day|5-hour"
```

Edit any line that describes the Codex cell as a single window to note it now shows the 5-hour and weekly windows (mirroring Claude). Keep it to one or two lines; do not restructure the docs.

- [ ] **Step 2: Sweep for em-dash / ellipsis (AGENTS.md §3.5).**

```bash
python -c "bad=[chr(0x2014),chr(0x2013),chr(0x2026)];[print(p,i) for p in ['README.md','docs/PRD.md'] for i,l in enumerate(open(p,encoding='utf-8'),1) if any(c in l for c in bad)] or print('clean')"
```

- [ ] **Step 3: Full validation matrix.**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace && cargo fmt --all -- --check && bun run check && bunx vitest run`

Also run the codex real-data smoke: `cargo run -p codex_local --example codex_local_smoke` (should print the snapshot with 5h + weekly windows on the maintainer's machine).

- [ ] **Step 4: Commit + PR.**

```bash
git add README.md docs/PRD.md
git commit -m "docs: note Codex column shows 5h and weekly windows"
git push -u origin feat/codex-usage-enrichment
gh pr create --title "feat(codex): show 5h + weekly windows and parse token/context internally" --body "Implements docs/superpowers/specs/2026-07-08-codex-usage-enrichment-design.md. Local-only Codex enrichment: expose 5h + weekly rate-limit windows (classified by duration), worst-window tray semantics, and parse token/context/credits as internal deferred data. No IPC schema change (serde-skip keeps the wire shape identical). Per-model Spark caps + credits balance are backend-only and out of scope."
```

---

## Self-Review

**Spec coverage:** Windows (5h + weekly) -> Tasks 2-5. Token/context/credits internal -> Task 1. `#[serde(skip)]` / no schema bump -> Task 1 (serialization test) + no `SNAPSHOT_SCHEMA_VERSION` edit anywhere. Classify-by-duration -> Task 1 (`kind`) + Task 2 (`codexWindowsByKind`). Tray worst-window -> Task 4. CLI/statusline -> Task 5. Docs/burn-limitation -> Task 1 (SCHEMA-NOTES) + Task 6. Out-of-scope (Spark, real credits, OAuth, schema bump) -> not implemented, documented in SCHEMA-NOTES. All spec sections map to a task.

**Placeholder scan:** No TBD/TODO; every code step shows complete code.

**Type consistency:** `codexQuota()`/`codexWindowsByKind()` names match between quota.ts and its consumers (GridView, CardsView). `five_hour()`/`weekly()`/`worst_window()`/`windows()`/`kind()` names match between codex_local (Task 1) and consumers (Tasks 4, 5). `CodexTokenUsage`/`CodexCredits` fields match between the type defs and the parser helpers. `codex_5h`/`codex_weekly` used consistently across the tray struct, `from_snapshot`, `worst()`, `codex_worst()`, and tests.
