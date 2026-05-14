# claude_cost

Pure-function Rust crate that computes the **estimated API-rate cost** of
Claude Code usage from `claude_parser::UsageEvent` slices and a vendored
Anthropic price table.

Sits in the same architectural slot as the `window` crate per AGENTS.md §4
boundary #2: no I/O on the hot path, no async, no logging above debug.

## What "cost" means in this crate

The output is a synthetic dollar figure — what your Claude Code sessions
WOULD cost if every event were billed at Anthropic's published direct-API
prices. Three honest framings depending on the user:

- **Pro / Max subscription user** (modal Claude Code user): this is NOT
  actual spend. Actual spend is the fixed monthly subscription fee. The
  figure is useful as a **"subscription leverage"** indicator — how much
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

- `compute_cost` is infallible. Unknown models go into
  `Cost.skipped_models`, never into an error.
- `load_bundled_prices` fails only if the compile-time-embedded JSON is
  malformed, contains a non-finite / negative / wildly-large price, or
  has zero model entries. Compile-time impossible for the data shipped
  here; the variant exists so the parser helper can be exercised on
  synthetic input in tests, and to leave a useful error path if a future
  refresh ever produces an unparseable file.
- **Price validation**: every parsed price must be finite, non-negative,
  and below `$1/token`. The `$1/token` upper bound is a typo guard — no
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

- `Cost.total_event_count` — every event the function saw (priced +
  skipped + unparsed). Use this for "events processed" metrics.
- `ModelCost.event_count` (per-row) — events whose model was found in
  the price table. Sum across rows gives the priced subset only.
- `Cost.unparsed_event_count` — events whose `event.model` was empty.
  `claude_parser` emits empty strings when the JSONL line omits the
  field; counted separately from `skipped_models` because it's a parser
  quirk, not a price-table gap.

## Provenance

`PRICE_TABLE_COMMIT` and `PRICE_TABLE_DATE` consts expose where and when
the vendored price snapshot came from. The build script `build.rs` parses
the data filename and emits both at compile time, so the const values and
the data file can never drift.

## Refresh procedure (manual, v0.1)

See `TODO-001` in `TODOS.md` (repo root) for the planned automation script.

1. Fetch `model_prices_and_context_window.json` from a chosen LiteLLM
   commit:
   ```
   COMMIT=<sha>
   curl -sf "https://raw.githubusercontent.com/BerriAI/litellm/$COMMIT/model_prices_and_context_window.json" \
     -o /tmp/litellm-full.json
   ```
2. Filter to bare `claude-*` keys (no vendor prefix, no slashes). Keep
   only the fields `claude_cost` reads:
   - `input_cost_per_token`, `output_cost_per_token`
   - `cache_creation_input_token_cost`, `cache_read_input_token_cost`
   - `max_input_tokens`, `max_output_tokens`, `litellm_provider` (kept
     for forward-compat / debugging; not consumed by code today)
3. Save to `data/litellm-prices-<commit-short>-<YYYYMMDD>.json` with a
   `_meta` block at the top describing `source`, `commit`, `fetched_at`,
   `filter`, and `license`.
4. Delete the old `data/litellm-prices-*.json` file.
5. **Update the `include_str!()` path in `src/prices.rs`** to match the
   new filename. The `build.rs` script auto-picks-up the new filename
   for the provenance consts, but `include_str!` requires a string
   literal and cannot be rewritten at build time.
6. Run `cargo build -p claude_cost && cargo test -p claude_cost` to
   verify everything still works.
7. Update `data/LICENSE-LITELLM` if upstream LICENSE changed.

## License

This crate's code is MIT (workspace default). The vendored price-table
data is also MIT, sourced from BerriAI/litellm; see
`data/LICENSE-LITELLM` for attribution.
