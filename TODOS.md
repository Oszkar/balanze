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

---

## TODO-003 — Uniformly redact serde error `Display` on the `ResponseShape` / JSON-parse paths

**What**: Run the serde error through `redact_for_display` (or use `e.classify()` instead of `{e}`) on the three top-level JSON-parse error sites: `anthropic_oauth/src/refresh.rs` (`refresh response: {e}`), `anthropic_oauth/src/client.rs:130` (`invalid JSON: {e}`), and `openai_client/src/client.rs:171`.

**Why**: A type-confused provider response (e.g. a 200 whose numeric field holds an `sk-…`-shaped string) makes serde's `Display` quote the offending value verbatim; that string flows into `OAuthError::ResponseShape` / the OpenAI equivalent, whose `Display` the CLI prints and logs. This is the exact leak class the team already hardened against for the nested `extra_usage` parse (`client.rs:157-168`, which logs only `e.classify()`). The top-level parse sites were never given the same treatment.

**Pros**:
- Closes a defense-in-depth secret-leak gap consistently across both HTTP clients.
- Removes the inconsistency where only the nested billing parse is hardened.

**Cons**:
- Low realistic probability (requires a provider to return a 200 with a type-confused secret).
- Touches two crates; wants its own small focused diff + a test per site.

**Context**:
- Surfaced by the Task 1 code-quality review (Track A v0.1.1, commit `034ee17`). The reviewer explicitly recommended a single uniform pass over all three sites rather than a one-off patch in `refresh.rs` (patching one site alone creates an inconsistency).
- Not a regression introduced by Task 1 — the pattern pre-exists at `client.rs:130` and in `openai_client`. Task 1 followed the established convention deliberately; this TODO is the convention-level fix.
- Preferred fix: mirror the `client.rs:157-168` precedent (`e.classify()` only) OR wrap with the existing `redact_for_display`. Add a test per site: a 200 body with a secret-shaped value in a wrong-typed field → assert the error string contains no `sk-…` and (if redactor used) the `sk-…REDACTED` marker.

**Depends on / blocked by**:
- Nothing. `redact_for_display` is already `pub(crate)` in `anthropic_oauth` (Task 1); `openai_client` has its own equivalent.

**Captured**: 2026-05-15, Track A v0.1.1 Task 1 code-quality review.
