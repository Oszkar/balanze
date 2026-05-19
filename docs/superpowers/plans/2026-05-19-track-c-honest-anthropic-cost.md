# Track C — Honest Anthropic API $ Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Balanze's Anthropic API-$ presentation honest: surface the real pay-as-you-go *extra-usage overage* (spike-verified, exact, billed) and hard-label the JSONL estimate as subscription-leverage-not-spend — without inventing data that doesn't exist.

**Architecture:** This is a **rendering + documentation** change, not a schema change. The spike (`~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md`) resolved that OAuth `extra_usage` raw integers are **cents** and the block is the claude.ai "Extra usage" pay-as-you-go overage meter — real money billed. `extra_usage` already rides in `Snapshot.claude_oauth.extra_usage` (an `Option<ExtraUsage>` already serialized into `--json`); `balanze_cli` merely *suppressed* it in human output (main.rs:838-839). So Track C: (1) un-suppress + render `extra_usage` honestly in `print_sections` and the compact grid; (2) sharpen the estimate's "leverage, NOT billed" framing so the real overage and the estimate can't be conflated; (3) fix the now-stale code docs (`anthropic_oauth` `ExtraUsage` doc says semantic "UNKNOWN"; `client.rs` provenance comment; `claude_cost` references a non-existent `Confidence::Estimated` enum) and sync the §8 docs — including correcting the **false PRD premise** that "Claude Code already records a per-event cost in the JSONL" (verified absent across 790 real files; the only Claude-provided real cost is the statusline session total, which is Track D's source).

**Scope explicitly excluded:** No per-event "Claude's own cost" figure (does not exist — disproven). No new `Confidence`/`DataSource` enum. No new `ExtraUsage`/`Snapshot`/`UsageEvent` fields (`resets_at`/`current_balance` are claude.ai-UI-only, absent from the OAuth wire response — see `RawExtraUsage`, client.rs:31-38). No watcher/predictor/statusline (Tracks D/E).

**Tech Stack:** Rust 2021, Cargo workspace. Crates touched: `anthropic_oauth` (doc + regression test), `balanze_cli` (rendering), `claude_cost` (doc fix). Docs: `docs/prd.md`, `README.md`, `CHANGELOG.md`, the single-user design doc + spike artifact under `~/.gstack/projects/balanze/`. Gates: `cargo fmt`/`clippy --workspace --all-targets -D warnings`/`test --workspace` (AGENTS.md §6); Conventional Commits (blocking `commit-msg` hook).

**AGENTS.md compliance:** §4 #1/#3 — only `anthropic_oauth` touches the OAuth wire format/`ExtraUsage`; `balanze_cli` consumes the typed struct, encodes no wire knowledge. §4 #8 — no `compose()`/`Snapshot` change, so CLI≡watcher parity is untouched. §2.1 — currency stays i64 micro-USD; rendering reuses the existing `micro_usd_to_display_dollars` helper. §8 — no schema change; the only deliberate doc change is correcting a factual error in the PRD (recorded in Task 5).

---

### Task 1: Resolve the `extra_usage` semantic in `anthropic_oauth` (doc + provenance comment + regression test)

**Goal:** Pin the now-resolved cents/overage semantic with a regression test and replace the "semantic UNKNOWN / do not trust" docs with the resolved truth.

**Files:**
- Modify: `crates/anthropic_oauth/src/types.rs:68-103` (the `ExtraUsage` struct doc)
- Modify: `crates/anthropic_oauth/src/client.rs:188-198` (the cents-conversion provenance comment)
- Test: `crates/anthropic_oauth/src/client.rs` `#[cfg(test)] mod tests` (add one `parse_response` regression test mirroring `parses_typical_max_user_response` at client.rs:301)

**Acceptance Criteria:**
- [ ] A new test `extra_usage_reconciled_cents_semantic` parses an `extra_usage` block of `{monthly_limit: 2500, used_credits: 2092, utilization: 83.7, currency: "USD", is_enabled: true}` and asserts `monthly_limit_micro_usd == 25_000_000`, `used_credits_micro_usd == 20_920_000` (cents ×10_000), `(utilization_percent - 83.7).abs() < 1e-3`, `is_enabled == true`.
- [ ] `ExtraUsage` struct doc no longer says the semantic is "UNKNOWN"/"should not be trusted"; it states: resolved 2026-05-19, raw ints are cents, this is the claude.ai "Extra usage" pay-as-you-go overage meter (real billed money), references the spike artifact path.
- [ ] `client.rs` provenance comment cites the first-hand spike reconciliation (not just the third-party hamed-elfayome cross-check).
- [ ] No struct fields added or renamed; `×10_000` conversion unchanged.

**Verify:** `cargo test -p anthropic_oauth` → all pass incl. the new test; `cargo clippy -p anthropic_oauth --all-targets -- -D warnings` → clean.

**Steps:**

- [ ] **Step 1: Add the regression test** in `crates/anthropic_oauth/src/client.rs`, inside `#[cfg(test)] mod tests`, after the existing `parses_typical_max_user_response` test:

```rust
#[test]
fn extra_usage_reconciled_cents_semantic() {
    // Regression pin for the 2026-05-19 reconciliation spike
    // (~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md).
    // claude.ai/settings/usage "Extra usage" showed $20.92 / $25.00 / 84%
    // for a Max-5x account; OAuth returned monthly_limit=2500,
    // used_credits=2092, utilization=83.7. Raw ints are CENTS. This test
    // fails if anyone reverts the cents (× 10_000) interpretation.
    let body = r#"{
        "five_hour": {"utilization": 10.0, "resets_at": "2026-05-19T18:00:00Z"},
        "extra_usage": {"is_enabled": true, "monthly_limit": 2500, "used_credits": 2092, "utilization": 83.7, "currency": "USD"}
    }"#;
    let snap = parse_response(body, None, Some("max".into()), None, fixed_ts()).unwrap();
    let eu = snap.extra_usage.expect("extra_usage present");
    assert!(eu.is_enabled);
    // 2500 cents = $25.00 = 25_000_000 micro-USD
    assert_eq!(eu.monthly_limit_micro_usd, 25_000_000);
    // 2092 cents = $20.92 = 20_920_000 micro-USD
    assert_eq!(eu.used_credits_micro_usd, 20_920_000);
    assert!((eu.utilization_percent - 83.7).abs() < 1e-3);
    assert_eq!(eu.currency, "USD");
}
```

- [ ] **Step 2: Run the test to confirm it passes** (this is a characterization/regression pin — the conversion is already correct, so it should be green; a RED here means the cents interpretation is broken):

Run: `cargo test -p anthropic_oauth extra_usage_reconciled_cents_semantic -- --nocapture`
Expected: `test ... ok` (1 passed). If it fails, STOP — the cents conversion regressed and that is the bug to fix before continuing.

- [ ] **Step 3: Replace the `ExtraUsage` struct doc** in `crates/anthropic_oauth/src/types.rs`. Replace the entire doc comment block spanning lines 68-93 (from `/// The `extra_usage` block — separate from cadence bars...` through `/// micro-USD assuming cents-input.`) with:

```rust
/// The `extra_usage` block — Anthropic's opt-in **pay-as-you-go overage**
/// meter (the claude.ai/settings/usage "Extra usage" section). Separate
/// from cadence bars because it is a billed-money counter, not a
/// utilization %.
///
/// **Semantic RESOLVED 2026-05-19** (spike:
/// `~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md`).
/// Raw `monthly_limit` / `used_credits` are integer **cents**; this block
/// is the claude.ai "Extra usage" pay-as-you-go overage meter — **real
/// money billed** beyond the subscription, exact, first-party. Reconciled
/// 3/3 against a Max-5x screenshot (`monthly_limit 2500 = $25.00`,
/// `used_credits 2092 = $20.92`, `utilization 83.7 ≈ "84% used"`). It is
/// NOT total spend and NOT the JSONL-derived subscription-leverage
/// estimate; `balanze_cli` renders it as a distinct REAL line, only when
/// `is_enabled`.
///
/// `resets_at` and the prepaid "current balance" are visible in the
/// claude.ai UI but are NOT in the OAuth wire response (see
/// `client.rs::RawExtraUsage`); only the five fields below exist on the
/// wire. Values are stored as i64 micro-USD (cents × 10_000) per
/// AGENTS.md §2.1.
```

- [ ] **Step 4: Update the provenance comment** in `crates/anthropic_oauth/src/client.rs`. Replace the comment lines 189-191 (`// Anthropic returns these in CENTS, not dollars. Confirmed by` / `// cross-checking against hamed-elfayome's Claude Usage Tracker` / `// (which shows the same numbers as $17.63 / $20.00). Convert`) with:

```rust
                    // Raw values are integer CENTS. Resolved first-hand by
                    // the 2026-05-19 reconciliation spike (Max-5x: OAuth
                    // 2500/2092 ↔ claude.ai "Extra usage" $25.00/$20.92/84%);
                    // see anthropic_oauth/src/types.rs ExtraUsage doc.
                    // Convert cents → micro-USD via × 10_000.
```

- [ ] **Step 5: Run gates and commit:**

Run: `cargo test -p anthropic_oauth && cargo clippy -p anthropic_oauth --all-targets -- -D warnings && cargo fmt -p anthropic_oauth -- --check`
Expected: all green.

```bash
git add crates/anthropic_oauth/src/types.rs crates/anthropic_oauth/src/client.rs
git commit -m "fix(anthropic_oauth): resolve extra_usage semantic (cents/overage) + regression test"
```

---

### Task 2: Surface `extra_usage` and hard-label the estimate in `print_sections`

**Goal:** Replace the `extra_usage` suppression with an honest EXTRA USAGE block and sharpen the `ANTHROPIC API COST` section so the estimate cannot be mistaken for billed money.

**Files:**
- Modify: `crates/balanze_cli/src/main.rs:838-839` (the suppression → render block)
- Modify: `crates/balanze_cli/src/main.rs:919-927` (the estimate header + Total label)

**Acceptance Criteria:**
- [ ] `print_sections` prints an `EXTRA USAGE (pay-as-you-go overage — REAL billed spend...)` block with spent/limit/utilization when `oauth.extra_usage.is_enabled`, and `EXTRA USAGE: disabled` when present-but-disabled; nothing when `extra_usage` is `None`.
- [ ] The dollar figures reuse `micro_usd_to_display_dollars` exactly as existing call sites (main.rs:926/943/1078) — no new formatting helper.
- [ ] The `ANTHROPIC API COST` header/Total explicitly reads ESTIMATE / "NOT money billed" and cross-references EXTRA USAGE.
- [ ] `cargo test --workspace` stays green (no stdout golden tests exist for these functions — verified: no test references the label strings).

**Verify:** `cargo build -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo test --workspace` → green. Manual eyeball: `cargo run -p balanze_cli -- --sections` (requires real `~/.claude` creds; confirm the EXTRA USAGE block renders and reads as REAL vs the ESTIMATE block).

**Steps:**

- [ ] **Step 1: Replace the suppression** at `crates/balanze_cli/src/main.rs`. Replace exactly these two lines (838-839):

```rust
        // extra_usage block intentionally suppressed; see commit e14365f.
        let _ = &oauth.extra_usage;
```

with:

```rust
        // Extra-usage = pay-as-you-go overage. Resolved 2026-05-19 spike:
        // raw ints are cents; this is the claude.ai "Extra usage" meter —
        // REAL billed money, distinct from the estimated API-rate figure
        // below. Only meaningful when the user enabled it.
        if let Some(eu) = &oauth.extra_usage {
            println!();
            if eu.is_enabled {
                println!(
                    "EXTRA USAGE (pay-as-you-go overage — REAL billed spend, from Anthropic OAuth):"
                );
                println!(
                    "  Spent this cycle:  {} of {} ({:.1}%)",
                    micro_usd_to_display_dollars(eu.used_credits_micro_usd),
                    micro_usd_to_display_dollars(eu.monthly_limit_micro_usd),
                    eu.utilization_percent
                );
                println!(
                    "  Real money billed beyond your subscription — NOT the estimate below."
                );
            } else {
                println!("EXTRA USAGE: disabled (no pay-as-you-go overage configured)");
            }
        }
```

- [ ] **Step 2: Hard-label the estimate.** In the same file, replace the `ANTHROPIC API COST` header `println!` (lines 919-923) and the Total `println!` (lines 924-927) with:

```rust
        println!(
            "ANTHROPIC API COST — ESTIMATE ONLY (JSONL × LiteLLM list-price @ {} / {}):",
            claude_cost::PRICE_TABLE_COMMIT,
            claude_cost::PRICE_TABLE_DATE,
        );
        println!(
            "  Est. list-price:   {} — subscription leverage, NOT money billed",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        );
        println!(
            "  (Real out-of-pocket spend, when enabled, is the EXTRA USAGE block above.)"
        );
```

- [ ] **Step 3: Confirm no golden test asserts these strings:**

Run: `rg -n "subscription leverage|est\. list-price|EXTRA USAGE|ESTIMATE ONLY" crates/*/tests crates/*/src/**/tests* 2>/dev/null || echo "no test references — safe"`
Expected: no test file matches (only source). If a test asserts old label text, update that assertion to the new text in this same task.

- [ ] **Step 4: Run gates and commit:**

Run: `cargo build -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo test --workspace && cargo fmt -p balanze_cli -- --check`
Expected: all green.

```bash
git add crates/balanze_cli/src/main.rs
git commit -m "feat(balanze_cli): surface extra-usage overage + hard-label the estimate in --sections"
```

---

### Task 3: Compact grid — lead with real overage, mark the estimate, fix the legend

**Goal:** In the one-line compact Anthropic API-$ cell, lead with the REAL overage when the user enabled it; always tag the estimate as leverage-not-billed; update the legend to name the overage.

**Files:**
- Modify: `crates/balanze_cli/src/main.rs:1069-1084` (`compact_anthropic_cost`)
- Modify: `crates/balanze_cli/src/main.rs:1037-1039` (the compact legend)

**Acceptance Criteria:**
- [ ] When `claude_oauth.extra_usage` is `Some` and `is_enabled`, the cell is `${spent}/${limit} overage billed · {estimate}`.
- [ ] When no enabled overage, the cell is exactly today's estimate string but with the suffix changed to `est-leverage (not billed)`.
- [ ] The legend names the overage as REAL pay-as-you-go spend.
- [ ] `cargo test --workspace` green.

**Verify:** `cargo build -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo test --workspace` → green. Manual: `cargo run -p balanze_cli` (compact) eyeball.

**Steps:**

- [ ] **Step 1: Replace `compact_anthropic_cost`** (lines 1069-1084) with:

```rust
fn compact_anthropic_cost(s: &Snapshot) -> String {
    // Real billed overage (only when the user enabled pay-as-you-go) leads;
    // the JSONL figure is ALWAYS tagged leverage-not-billed so a ~$4,000
    // estimate is never read as ~$4,000 of real spend.
    let overage = s
        .claude_oauth
        .as_ref()
        .and_then(|o| o.extra_usage.as_ref())
        .filter(|eu| eu.is_enabled)
        .map(|eu| {
            format!(
                "{}/{} overage billed",
                micro_usd_to_display_dollars(eu.used_credits_micro_usd),
                micro_usd_to_display_dollars(eu.monthly_limit_micro_usd)
            )
        });
    let est = match (&s.anthropic_api_cost, &s.anthropic_api_cost_error) {
        (Some(cost), _) if cost.total_event_count == 0 => "○ no jsonl data yet".to_string(),
        (Some(cost), _) => format!(
            "~{} est-leverage (not billed)",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        ),
        (None, Some(_)) => "✗ cost synthesis failed".to_string(),
        (None, None) if s.claude_jsonl_error.is_some() => "✗ jsonl load failed".to_string(),
        (None, None) => "○ no jsonl data".to_string(),
    };
    match overage {
        Some(o) => format!("{o} · {est}"),
        None => est,
    }
}
```

- [ ] **Step 2: Update the legend.** Replace the three legend `println!` lines (1037-1039):

```rust
    println!("Quota % = live server-reported utilization. API $: Anthropic =");
    println!("estimated list-price for local Claude Code tokens (subscription");
    println!("leverage — NOT money you were billed); OpenAI = real billed spend.");
```

with:

```rust
    println!("Quota % = live server-reported utilization. API $: Anthropic =");
    println!("estimated list-price for local Claude Code tokens (subscription");
    println!("leverage — NOT billed). 'overage billed' = REAL pay-as-you-go");
    println!("spend from Anthropic. OpenAI = real billed spend.");
```

- [ ] **Step 3: Run gates and commit:**

Run: `cargo build -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo test --workspace && cargo fmt -p balanze_cli -- --check`
Expected: all green.

```bash
git add crates/balanze_cli/src/main.rs
git commit -m "feat(balanze_cli): compact cell leads with real overage, marks estimate as leverage"
```

---

### Task 4: Fix the stale `Confidence::Estimated` reference in `claude_cost`

**Goal:** `claude_cost` docs reference a `Confidence::Estimated` enum that does not exist anywhere in the workspace. Make the prose accurate (Balanze labels are rendered prose, not a type).

**Files:**
- Modify: `crates/claude_cost/src/lib.rs:15-16` (the `//! Either way, [`Cost`] outputs are marked `Confidence::Estimated`...` line)

**Acceptance Criteria:**
- [ ] No `claude_cost` doc claims a `Confidence::Estimated` type/enum.
- [ ] The replacement states the estimate is labeled as such by the render layer (`balanze_cli`), citing AGENTS.md §2.1.
- [ ] `cargo test -p claude_cost` green; `cargo doc -p claude_cost --no-deps` builds without intra-doc-link warnings for this line.

**Verify:** `cargo test -p claude_cost && cargo clippy -p claude_cost --all-targets -- -D warnings` → green.

**Steps:**

- [ ] **Step 1: Replace** the two doc lines in `crates/claude_cost/src/lib.rs` (currently):

```rust
//! Either way, [`Cost`] outputs are marked `Confidence::Estimated` by the
//! Balanze data layer's convention (see AGENTS.md §2.1).
```

with:

```rust
//! Either way, [`Cost`] is an **estimate**, never billed spend. There is
//! no `Confidence` type in the workspace; the render layer (`balanze_cli`)
//! labels this figure "estimate / subscription leverage / not billed" in
//! every surface, per AGENTS.md §2.1 and the `Cost::total_micro_usd` doc.
```

- [ ] **Step 2: Run gates and commit:**

Run: `cargo test -p claude_cost && cargo clippy -p claude_cost --all-targets -- -D warnings && cargo fmt -p claude_cost -- --check`
Expected: all green.

```bash
git add crates/claude_cost/src/lib.rs
git commit -m "docs(claude_cost): drop reference to non-existent Confidence::Estimated enum"
```

---

### Task 5: Doc sync — correct the false PRD premise, reframe Track C, update README/CHANGELOG/design doc/spike artifact (AGENTS.md §8)

**Goal:** Make the product/architecture docs match reality: the "Claude Code records a per-event cost in the JSONL" premise is false; Track C is the reframed estimate-honesty + overage-surfacing change; `extra_usage` is no longer an open issue.

**Files:**
- Modify: `docs/prd.md:271-275` (Track C bullets) and `docs/prd.md:323` (Open Questions `extra_usage` line)
- Modify: `README.md` (the `extra_usage` "suppressed / unclear semantics" Known-issues bullet)
- Modify: `CHANGELOG.md` (`[Unreleased]` section — add the Track C entry)
- Modify: `~/.gstack/projects/balanze/oszka-main-design-20260514-153159.md` (Track C section, if it carries one) and `~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md` (correct the schema line: 5 wire fields only, no resets_at/balance; note per-event-cost premise disproven)

**Acceptance Criteria:**
- [ ] `docs/prd.md` Track C no longer asserts Claude Code JSONL carries a per-event cost; it states that premise was disproven (790 files, 2026-05-19), that the real Claude cost lives in the statusline (Track D), and that Track C = estimate-honesty relabel + spike-verified extra-usage overage surfacing. The Open-Questions `extra_usage` line is marked RESOLVED with the spike date.
- [ ] `README.md` Known issues: the `extra_usage` bullet is replaced with a one-liner that it's now surfaced as real overage (or removed if no longer an "issue").
- [ ] `CHANGELOG.md` `[Unreleased]` has a Track C entry under `### Added`/`### Changed`/`### Fixed` describing the user-visible change (overage now shown; estimate hard-labeled) without internal jargon.
- [ ] The spike artifact's schema line is corrected to "5 verified wire fields only (no resets_at/current_balance)"; a note records the per-event-cost premise was false and Track C reframed accordingly.
- [ ] Internal doc cross-references still resolve (AGENTS.md §6 docs row).

**Verify:** `git grep -n "per-event cost" docs/prd.md` shows no surviving false claim; `cargo test --workspace` unaffected (docs only); manual read of the four docs for cross-link integrity.

**Steps:**

- [ ] **Step 1: Rewrite `docs/prd.md` Track C bullet 1** (line 273, "Primary figure becomes Claude Code's own pre-calculated cost…"). Replace that bullet with:

```markdown
- **Make the estimate honest + surface the real overage.** The earlier premise that "Claude Code records a per-event cost in the JSONL Balanze parses" is **false for current Claude Code** — verified absent across 790 real session files (2026-05-19); Anthropic removed `costUSD` from transcripts, which is why ccusage et al. recompute from tokens. The only Claude-provided *real* cost is the statusline session total — that is **Track D's** source, not this one. So Track C instead: (a) hard-labels the JSONL × list-price figure as "estimate / subscription leverage / NOT billed" so it can't read as real spend, and (b) surfaces the **real pay-as-you-go extra-usage overage** (next bullet). No new data source; rendering + docs only (no `Snapshot`/parsed-event schema change — `extra_usage` already rides in the snapshot).
```

- [ ] **Step 2: Rewrite `docs/prd.md` Track C bullet 2** (line 274, the reconciliation-spike bullet). Replace with:

```markdown
- **Reconciliation spike on the OAuth `extra_usage` block — RESOLVED 2026-05-19.** The spike (`~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md`) proved `extra_usage` raw ints are **cents** and the block is the claude.ai "Extra usage" pay-as-you-go **overage** meter — real money billed, exact, first-party (reconciled 3/3 against a Max-5x screenshot). It is promoted to a real overage line in CLI output (only when the user enabled it), explicitly distinct from the estimate. It is NOT "total spend this month" (no such $ surface exists for a Max user; Phase-0 admin spike already NO-GO).
```

- [ ] **Step 3: Update `docs/prd.md` Open Questions** (line 323, the `extra_usage` clause). Change the clause "a bounded spike reconciles the OAuth `extra_usage` block against the live usage UI to decide whether it can be promoted to a real "spend this month" figure" to:

```markdown
the OAuth `extra_usage` block was reconciled (RESOLVED 2026-05-19): it is the claude.ai pay-as-you-go overage meter (cents, exact, real billed money) and is surfaced as such — not "spend this month"
```

- [ ] **Step 4: Update `README.md` Known issues.** Replace the `**\`extra_usage\` block from OAuth suppressed.**` bullet (the paragraph beginning "Anthropic's OAuth response returns a `monthly_limit / used_credits` block whose semantics don't reconcile…") with:

```markdown
- **Extra-usage overage is now surfaced (resolved).** The OAuth
  `extra_usage` block was reconciled (2026-05-19): raw values are cents and
  it is the claude.ai pay-as-you-go *overage* meter — real billed money.
  Balanze shows it as a distinct REAL line (only when you enabled extra
  usage), separate from the JSONL list-price *estimate*. There is no
  "total spend this month" API for Max users; that remains by design.
```

- [ ] **Step 5: Add the `CHANGELOG.md` entry.** Under `## [Unreleased]`, replace the `_Nothing yet…_` placeholder with:

```markdown
### Added
- **Real pay-as-you-go overage surfaced.** If you enabled Anthropic
  "Extra usage", `balanze-cli` now shows your real billed overage
  (spent / limit / %) in both the compact grid and `--sections` — the
  exact figure claude.ai shows. Previously suppressed because its units
  were unverified; a reconciliation spike resolved it (cents; the
  claude.ai overage meter).

### Changed
- **The Anthropic API-$ estimate is now hard-labeled.** The JSONL ×
  list-price number is explicitly tagged "estimate — subscription
  leverage, NOT billed" and visually separated from the real overage, so
  a large estimate can't be misread as real spend.

### Fixed
- `anthropic_oauth` `ExtraUsage` docs no longer say the semantic is
  "unknown" (resolved: cents / overage meter); `claude_cost` no longer
  references a non-existent `Confidence::Estimated` type. Corrected the
  PRD's false "Claude Code records a per-event cost in the JSONL" premise.
```

- [ ] **Step 6: Correct the spike artifact.** In `~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md`, in the "Decision" section, change the field-list sentence so it reads that only the **five wire fields** (`is_enabled, monthly_limit, used_credits, utilization, currency`) are promoted, and add: "`resets_at` / `current_balance` are claude.ai-UI-only and absent from the OAuth response (`RawExtraUsage`) — excluded." Add a one-line note under "Net effect": "Track C's other half (per-event Claude cost as primary) was disproven during planning — current Claude Code JSONL carries no per-event cost (790 files); that goal moves to Track D (statusline). Track C reframed to estimate-honesty + overage surfacing."

- [ ] **Step 7: Sync the design doc.** In `~/.gstack/projects/balanze/oszka-main-design-20260514-153159.md`, locate the Track C / Phase-2 section (if present) and align it with the reframed scope (mirror the prd.md bullets from Steps 1-2). If the design doc has no Track C subsection, add a one-paragraph "Track C (reframed 2026-05-19)" note pointing at the spike artifact and the prd.md bullets. (Read the file first to find the insertion point.)

- [ ] **Step 8: Verify cross-references and commit:**

Run: `git grep -n "per-event cost" docs/prd.md ; cargo test --workspace`
Expected: no surviving false "Claude Code records a per-event cost" claim in prd.md; workspace tests green (docs-only change).

```bash
git add docs/prd.md README.md CHANGELOG.md
git commit -m "docs: reframe Track C (per-event-cost premise disproven), resolve extra_usage issue"
```
(The `~/.gstack/...` design doc + spike artifact are single-user and outside the repo — they are not staged; edits there are saved in place.)

---

### Task 6: Full workspace validation gate

**Goal:** Prove the whole Track C change is green against the AGENTS.md §6 matrix before handing off.

**Files:** none (verification only).

**Acceptance Criteria:**
- [ ] `cargo fmt --all -- --check` clean.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `cargo test --workspace` all pass (incl. Task 1's regression test and the existing `integration_4quadrant.rs` suite unchanged — proves no Snapshot/serde drift).
- [ ] `bun run check` clean (no frontend change; cheap parity check).
- [ ] Manual eyeball recorded: `cargo run -p balanze_cli -- --sections` and `cargo run -p balanze_cli` show the EXTRA USAGE block (or "disabled"/absent) and the hard-labeled estimate, distinct and not conflatable.

**Verify:** the four commands above, each exits 0; manual output pasted into the task close notes.

**Steps:**

- [ ] **Step 1: Run the full gate:**

Run:
```bash
cargo fmt --all -- --check && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo test --workspace && \
bun run check
```
Expected: all four succeed. If `integration_4quadrant.rs` fails, STOP — the change unexpectedly altered the Snapshot/serde shape (it must not).

- [ ] **Step 2: Manual render check** (requires real `~/.claude` creds; the OAuth endpoint may 429 — if so, note it and rely on the unit/regression coverage, do not block):

Run: `cargo run -p balanze_cli -- --sections` then `cargo run -p balanze_cli`
Expected: `--sections` shows `EXTRA USAGE (...REAL billed spend...)` (or `disabled`) and `ANTHROPIC API COST — ESTIMATE ONLY`; compact shows `… overage billed · ~$… est-leverage (not billed)` (or just the est cell when overage off). Paste the (redacted-as-needed) output into the close notes.

- [ ] **Step 3: No commit** (verification task). If any gate fails, return to the owning task; do not mark complete with a failing gate (AGENTS.md §7).

---

## Self-Review

**Spec coverage:** (i) extra_usage promotion → Tasks 2 (sections) + 3 (compact); (ii) hard transparency relabel → Tasks 2 + 3; (iii) defect fixes → Task 1 (anthropic_oauth doc/comment/test) + Task 4 (claude_cost doc) + Task 5 (PRD/README/CHANGELOG/design/spike). Validation → Task 6. No spec item unmapped.

**Placeholder scan:** every code step shows the exact replacement text; every doc step shows the exact prose; no "TBD/handle appropriately". The only non-verbatim step is Task 5 Step 7 (design-doc sync) which requires reading the single-user file first to find the insertion point — explicitly instructed, not a placeholder.

**Type/name consistency:** `oauth.extra_usage` / `ExtraUsage { is_enabled, used_credits_micro_usd, monthly_limit_micro_usd, utilization_percent, currency }` (matches types.rs:94-103); `micro_usd_to_display_dollars(i64) -> String` (existing helper, call sites main.rs:926/943/1078); `compact_anthropic_cost`, `print_sections`, `snapshot.claude_oauth`, `snapshot.anthropic_api_cost` — all match the read source. No new types introduced.

**Risk note:** the manual render checks need real OAuth creds and the `/api/oauth/usage` endpoint 429s Balanze per-account (observed in the spike). Mitigation baked into Task 6 Step 2: unit/regression coverage (Task 1) is the gate; manual eyeball is best-effort, non-blocking.
