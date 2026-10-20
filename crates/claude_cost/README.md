# claude_cost

Pure-function Rust crate that computes the **estimated API-rate cost** of
Claude Code usage from `claude_parser::UsageEvent` slices and a vendored
Anthropic price table.

Sits in the same architectural slot as the `window` crate per `docs/ARCHITECTURE.md` boundary #2: no I/O on the hot path, no async, no logging above debug.

## What "cost" means in this crate

The output is a synthetic dollar figure - what your Claude Code sessions
WOULD cost if every event were billed at Anthropic's published direct-API
prices. Three honest framings depending on the user:

- **Pro / Max subscription user** (modal Claude Code user): this is NOT
  actual spend. Actual spend is the fixed monthly subscription fee. The
  figure is useful as a **"subscription leverage"** indicator - how much
  API-rate value you extracted from the plan this month.
- **Direct-API user** (rare for Claude Code): approximates actual spend,
  modulo vendored-price-table freshness.
- **Mixed user**: most Claude Code usage flows through the subscription;
  this figure tracks the subscription portion only.

UIs rendering `Cost.total_micro_usd` MUST label it with this framing
(e.g. "estimated API-rate cost", "subscription leverage", "what your usage
would cost at API rates"). Presenting it as "total spend" to a Max-plan
user is actively misleading.

## Public API

```rust
use claude_cost::{compute_cost, load_bundled_prices};

let prices = load_bundled_prices()?;
let cost = compute_cost(&events, &prices);
println!(
    "Estimated API-rate cost: ${:.2}",
    cost.total_micro_usd as f64 / 1_000_000.0,
);
```

- `compute_cost` is infallible. Unknown models that carried usage go into
  `Cost.skipped_models`, never into an error. An unknown model seen only on
  zero-usage events (Claude Code's `<synthetic>` turns) is not listed, since
  nothing was left unpriced.
- `load_bundled_prices` fails only if the compile-time-embedded JSON is
  malformed, contains a non-finite / negative / wildly-large price, or
  has zero model entries. Compile-time impossible for the data shipped
  here; the variant exists so the parser helper can be exercised on
  synthetic input in tests, and to leave a useful error path if a future
  refresh ever produces an unparseable file.
- **Price validation**: every parsed price must be finite, non-negative,
  and below `$1/token`. The `$1/token` upper bound is a typo guard - no
  real Anthropic price is anywhere near (Opus is `$75/M = 7.5e-5/token`).
- Currency math: `i64` micro-USD outputs, `i64` nano-USD per-token
  storage, `i128` intermediate products to avoid overflow. Saturates at
  `i64::MAX` / `i64::MIN` rather than panicking. **Caveat**: at saturation,
  the grand-total `total_micro_usd` caps at `i64::MAX`, so summing
  `per_model.iter().map(|m| m.total_micro_usd)` may exceed
  `total_micro_usd`. Callers should not assert equality at saturation.

## Event counting

`Cost` exposes three event-count signals because "how many events did we
see?" has three honest answers:

- `Cost.total_event_count` - every event the function saw (priced +
  skipped + unparsed). Use this for "events processed" metrics.
- `ModelCost.event_count` (per-row) - events whose model was found in
  the price table. Sum across rows gives the priced subset only.
- `Cost.unparsed_event_count` - events whose `event.model` was empty.
  `claude_parser` emits empty strings when the JSONL line omits the
  field; counted separately from `skipped_models` because it's a parser
  quirk, not a price-table gap.

The desktop uses these counts to label partial estimates and show missing model prices or names. If the priced subset is empty, it displays an unavailable estimate instead of a misleading `$0.00`. A zero total with priced events remains a valid estimate.

## Provenance

`PRICE_TABLE_COMMIT` and `PRICE_TABLE_DATE` consts expose where and when
the vendored price snapshot came from. The build script `build.rs` parses
the data filename and emits both at compile time, so the const values and
the data file can never drift.

The bundled snapshot was fetched on 2026-10-15 from [LiteLLM commit 8302f00](https://github.com/BerriAI/litellm/blob/8302f006ddf5b44b13baf17e2673c0fceb826725/model_prices_and_context_window.json). Current model rates were cross-checked against [Anthropic's pricing](https://platform.claude.com/docs/en/about-claude/pricing). It includes Opus 5 and 5.5, Sonnet 5 and 5.5, Fable 5.1 and Mythos models, and Sonnet 5's permanent $2/$10 per million input/output tokens. Opus 5.5 and Sonnet 5.5 cache reads are 0.05x base input. Haiku 5.5 is priced in two tiers split at 100,000 prompt tokens: a request whose prompt (input + cache writes + cache reads) exceeds that is billed entirely at the higher rates, chosen per event (`ModelPrices::long_prompt`, `LONG_PROMPT_THRESHOLD_TOKENS`). Historical entries remain available for exports, including ones upstream has dropped (carried forward unchanged). Estimates use this snapshot's list rates for all events, using each event's 5-minute/1-hour cache-write breakdown when available; they do not reconstruct historical invoices. The 1-hour write rate is derived from Anthropic's published 2x base-input multiplier. Legacy events without a breakdown retain the 5-minute estimate. Duration components are summed in nano-USD before rounding once to micro-USD.

## Refresh procedure

Use the maintainer-only `scripts/refresh-claude-prices.mjs` from the repository root. It requires Node.js with built-in `fetch` (the repo's current Node toolchain suffices), accepts a full lowercase 40-character LiteLLM commit SHA, and performs no runtime or scheduled fetching.

1. Choose a LiteLLM commit and preview it with `just refresh-claude-prices <full-sha> --dry-run`. The equivalent direct invocation is `node scripts/refresh-claude-prices.mjs <full-sha> --dry-run`. Review the added models, missing upstream models, and changed rates.
2. Missing keys fail by default without changing files. After reviewing them, add `--retain-missing` to carry historical entries forward unchanged. The current snapshot already includes retired upstream models, so this option will normally be necessary. `_meta.retained_models` records the previous snapshot's commit and fetch date for each retained entry, preserving that attribution on subsequent refreshes. For legacy snapshots without per-model provenance, this identifies the snapshot the entry was copied from, not necessarily its original upstream commit.
3. Remove `--dry-run` to publish. The script validates rates, writes `data/litellm-prices-<commit-short>-<YYYYMMDD>.json` using the current UTC date, updates the single matching `include_str!` in `src/prices.rs`, and removes the old snapshot. `build.rs` derives the provenance constants from the filename. Run one refresh at a time in a clean checkout. Ordinary publication failures attempt rollback; a process crash or rollback failure may require restoring the snapshot and `prices.rs` from git before retrying.
4. Cross-check current model rates against [Anthropic's pricing](https://platform.claude.com/docs/en/about-claude/pricing), including cache reads, the 5-minute cache-write rate, above-100k tiers, and the 2x base-input multiplier used for 1-hour writes. The script deliberately marks the snapshot as unverified. After checking, add `_meta.verified_against`, replace `_meta.verification_note` with what was verified, update this README's provenance paragraph, and add or update assertions for newly supported or changed rates in `src/prices.rs`. New pricing structures require a separate code change; the script only preserves the fields supported today.
5. Review upstream `LICENSE` at the chosen commit and update `data/LICENSE-LITELLM` if needed.
6. Run `bun run test -- scripts/refresh-claude-prices.test.ts`, then `cargo build -p claude_cost` and `cargo test -p claude_cost`. A refresh changes a Rust source path, so also run the repository's required Rust gates: `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace`, and `cargo fmt --all -- --check`. Review the complete git diff before committing.

The filter keeps bare `claude-*` entries with `litellm_provider: "anthropic"`, excluding slash-prefixed and colon-bearing provider variants. It retains input/output and cache creation/read rates, their four non-batch `*_above_100k_tokens` variants (a tier requires input and output), and the informational `max_input_tokens`, `max_output_tokens`, and `litellm_provider` fields. Tests use local fixtures and never contact providers.

## License

This crate's code is MIT (workspace default). The vendored price-table
data is also MIT, sourced from BerriAI/litellm; see
`data/LICENSE-LITELLM` for attribution.
