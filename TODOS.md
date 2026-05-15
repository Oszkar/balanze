# TODOS

Deferred work items. Captured during reviews so the reasoning doesn't evaporate.

Each item:

- **What**: one-line description.
- **Why**: the concrete problem it solves.
- **Pros / Cons**: what's gained vs. what's costed.
- **Context**: enough detail that picking it up in 3 months still makes sense.
- **Depends on / blocked by**: prerequisites.
- **Captured**: when + by which review.

> **Promoted to the roadmap:** TODO-002 (criterion benchmarks for `claude_cost`
> + `codex_local`) is no longer a loose deferred item — it is now scheduled in
> `docs/prd.md` Phase 2 (v0.2 Track E), riding with the watcher so the live
> refresh cadence has a measured performance budget. Removed here on
> 2026-05-15 (reference-review roadmap consolidation) to avoid two sources of
> truth.

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
- Note (2026-05-15): v0.2 Track C demotes the LiteLLM recompute to a *diagnostic fallback* (Claude Code's own pre-calculated cost becomes primary). This lowers TODO-001's priority but does not remove it — the fallback path still needs a current table for events lacking a pre-calculated cost.

**Depends on / blocked by**:
- claude_cost crate exists (v0.1).
- Not blocked on anything else; can be added in any later release.

**Captured**: 2026-05-14, /plan-eng-review session for v0.1 claude_cost + codex_local.
