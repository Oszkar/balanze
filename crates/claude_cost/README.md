# claude_cost

Pure-function Rust crate that synthesizes estimated Claude API cost from
`claude_parser::UsageEvent` slices and a vendored Anthropic price table.

Sits in the same architectural slot as the `window` crate per AGENTS.md §4
boundary #2: no I/O on the hot path, no async, no logging above debug.

## Public API

```rust
use claude_cost::{compute_cost, load_bundled_prices};

let prices = load_bundled_prices()?;
let cost = compute_cost(&events, &prices);
println!(
    "Anthropic API spend: ${:.2}",
    cost.total_micro_usd as f64 / 1_000_000.0,
);
```

- `compute_cost` is infallible. Unknown models go into
  `Cost.skipped_models`, never into an error.
- `load_bundled_prices` fails only if the compile-time-embedded JSON is
  malformed (impossible for the data shipped here; the variant exists so
  the parser helper can be exercised on synthetic input in tests, and to
  leave a useful error path if a future refresh ever produces an
  unparseable file).
- Currency math: `i64` micro-USD outputs, `i64` nano-USD per-token
  storage, `i128` intermediate products to avoid overflow. Saturates at
  `i64::MAX` / `i64::MIN` rather than panicking.

## Provenance

`PRICE_TABLE_COMMIT` and `PRICE_TABLE_DATE` consts expose where and when
the vendored price snapshot came from. The build script `build.rs` parses
the data filename and emits both at compile time, so the const values and
the data file can never drift.

## Refresh procedure (manual, v0.1)

See `TODOS-001` in repo root for the planned automation script.

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
