# TODOS

Deferred work items. Captured during reviews so the reasoning doesn't evaporate.

Each item:

- **What**: one-line description.
- **Why**: the concrete problem it solves.
- **Pros / Cons**: what's gained vs. what's costed.
- **Context**: enough detail that picking it up in 3 months still makes sense.
- **Depends on / blocked by**: prerequisites.
- **Captured**: when + by which review.

---

## TODO-001 — Refresh script for the vendored LiteLLM price table

**What**: A script (`scripts/refresh-claude-prices.sh` or `.py`) that mechanizes refreshing `crates/claude_cost/data/litellm-prices-*.json`.

**Why**: v0.1 ships a vendored snapshot from a specific LiteLLM commit. Anthropic ships new models periodically. The refresh procedure today is informal ("fetch the latest JSON, jq-filter, save, update Cargo.toml"). Six months from now I won't remember the exact filter step. Mechanizing it removes a footgun.

**Pros**:
- Refresh becomes a one-command operation (`./scripts/refresh-claude-prices.sh`).
- v0.2's price-refresh story (runtime fetch + cache) builds on a known-good baseline.
- Reviewers can rerun the script and diff the output instead of trusting hand-vendored data.

**Cons**:
- Extra script to maintain (~50 lines bash or ~100 lines Python).
- Cron-style automation is out of scope; user still has to run it before every release.

**Context**:
- Vendored data lives at `crates/claude_cost/data/litellm-prices-<commit>-<date>.json`.
- Source: https://github.com/BerriAI/litellm — `model_prices_and_context_window.json`.
- Filter to `claude-*` keys (Anthropic subset only, ~5KB after filtering).
- After refresh: update `Cargo.toml`'s `include_str!` path (or symlink), regenerate provenance const via `build.rs`, run `cargo test -p claude_cost`.
- The script lives in `scripts/` (workspace-level), not inside the crate.

**Depends on / blocked by**:
- claude_cost crate exists (v0.1).
- Not blocked on anything else; can be added in any later release.

**Captured**: 2026-05-14, /plan-eng-review session for v0.1 claude_cost + codex_local.

---

## TODO-002 — Performance benchmarks for claude_cost + codex_local

**What**: criterion-based benchmarks for `claude_cost::compute_cost` and `codex_local::parse_str`/`dedup_events`/`IncrementalParser`.

**Why**: Both crates are O(events) and currently presumed fast. When v0.2's `watcher` arrives, the 30s tray repaint cadence needs both crates to run in well under 30s on realistic data. We have no baseline today. A regression in v0.3 (e.g., adding logging in a hot loop) could degrade performance silently. /benchmark consumes these to track trends across PRs.

**Pros**:
- Quantitative answer to "is the parser fast enough?" — gut feel becomes a graph.
- /benchmark skill detects regressions automatically.
- v0.2 watcher work has measurable performance budget.

**Cons**:
- criterion runs are slow in CI (~30s each); adds ~1 min to total CI time.
- Pre-empts the question before there's real evidence either crate is slow (YAGNI risk).
- Maintenance: benchmark code can rot if not run regularly.

**Context**:
- Targets:
  - `claude_cost::compute_cost` over 10K / 100K / 1M synthetic UsageEvents.
  - `codex_local::parse_str` over a 10MB / 100MB fixture JSONL file.
  - `codex_local::IncrementalParser` cycle (first-read + append + skip-unchanged).
- Crate layout: `crates/claude_cost/benches/`, `crates/codex_local/benches/`.
- criterion is already a likely dev-dep (verify with `cargo tree`).
- Run with `cargo bench --workspace`.
- CI gate: not required (don't fail PR on regression); informational via /benchmark only.

**Depends on / blocked by**:
- Both crates exist (v0.1).
- /benchmark skill is set up (check `gstack` skill availability).
- Best added when v0.2 watcher work starts (gives the benchmark a clear consumer).

**Captured**: 2026-05-14, /plan-eng-review session for v0.1 claude_cost + codex_local.
