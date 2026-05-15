//! Smoke test against the user's real `~/.claude/projects/` data + the
//! vendored Anthropic price table.
//!
//! Run with:
//!   cargo run --release -p claude_cost --example cost_smoke
//!
//! Prints the same kind of summary the eventual CLI / tray UI will surface:
//! event totals, per-model cost breakdown sorted desc, grand total in
//! dollars + micro-USD, any skipped models (in the JSONL but absent from
//! the price table), any unparsed events (JSONL omitted the model field),
//! plus price-table provenance for traceability.
//!
//! Manual-test playbook for the maintainer:
//! 1. Run the example. Verify the file count matches what's actually under
//!    `~/.claude/projects/`.
//! 2. Verify "Grand total" is plausible vs. your subjective sense of usage.
//! 3. Verify "Skipped models" is empty (or surfaces a brand-new model name
//!    that genuinely isn't in the vendored table yet — that's a real find).
//! 4. Verify "Unparsed events" is small or zero (claude_parser only emits
//!    these when the JSONL line legitimately omits the model field).
//! 5. Verify the per-model breakdown's totals add up to the grand total
//!    (within saturation bounds — see `Cost` docs).

use std::fs;

use claude_cost::{compute_cost, load_bundled_prices, PRICE_TABLE_COMMIT, PRICE_TABLE_DATE};
use claude_parser::{dedup_events, find_jsonl_files, parse_str};

fn main() -> anyhow::Result<()> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("could not resolve user's base directories"))?;
    let claude_dir = base_dirs.home_dir().join(".claude").join("projects");

    println!("Scanning {}", claude_dir.display());

    if !claude_dir.exists() {
        anyhow::bail!(
            "~/.claude/projects/ does not exist on this machine. Install Claude Code \
             and run it at least once to populate the directory."
        );
    }

    let files = find_jsonl_files(&claude_dir)?;
    println!("Found {} JSONL files", files.len());

    if files.is_empty() {
        println!("No JSONL files found — nothing to compute cost for.");
        return Ok(());
    }

    let mut all_events = Vec::new();
    let mut parse_error_files = 0_usize;
    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("read failed for {}: {e}", path.display());
                continue;
            }
        };
        match parse_str(&content) {
            Ok(events) => all_events.extend(events),
            Err(e) => {
                eprintln!("parse error in {}: {e}", path.display());
                parse_error_files += 1;
            }
        }
    }

    let raw_count = all_events.len();
    // Claude Code emits each assistant message multiple times; without dedup
    // this example over-counts ~2x vs the real CLI path (which dedups in
    // `load_and_dedup_claude_events`). Dedup here so the smoke total
    // reconciles with `balanze-cli` and with what Anthropic actually sees.
    dedup_events(&mut all_events);
    println!(
        "Parsed {raw_count} raw events ({parse_error_files} files had parse errors); \
         {} after (message_id, request_id) dedup",
        all_events.len()
    );

    let prices = load_bundled_prices()?;
    let cost = compute_cost(&all_events, &prices);

    println!();
    println!("=== Price table provenance ===");
    println!("LiteLLM commit:    {PRICE_TABLE_COMMIT}");
    println!("Vendored on:       {PRICE_TABLE_DATE}");
    println!("Models in table:   {}", prices.models.len());

    println!();
    println!("=== Event counts ===");
    println!("Total events seen:      {}", cost.total_event_count);
    let priced_event_count: usize = cost.per_model.iter().map(|m| m.event_count).sum();
    println!("Priced events:          {priced_event_count}");
    println!(
        "Unknown-model events:   {}",
        cost.total_event_count - priced_event_count - cost.unparsed_event_count
    );
    println!("Empty-model events:     {}", cost.unparsed_event_count);

    let dollars = cost.total_micro_usd as f64 / 1_000_000.0;
    println!();
    println!("=== Estimated API-rate cost ===");
    println!("${dollars:.4}  ({} micro-USD)", cost.total_micro_usd);
    println!();
    println!("NOTE: This is what your Claude Code usage WOULD cost if billed at");
    println!("Anthropic's direct API rates. If you're on a Pro/Max subscription,");
    println!("your actual spend is the fixed monthly fee — this number is a");
    println!("\"subscription leverage\" indicator (how much API-equivalent value");
    println!("you're getting from the plan). For direct-API users (no subscription),");
    println!("this approximates your actual spend, modulo price-table freshness.");

    if !cost.per_model.is_empty() {
        println!();
        println!("=== Per-model breakdown (sorted by total, desc) ===");
        for m in &cost.per_model {
            let total = m.total_micro_usd as f64 / 1_000_000.0;
            let input = m.input_micro_usd as f64 / 1_000_000.0;
            let output = m.output_micro_usd as f64 / 1_000_000.0;
            let cache_create = m.cache_creation_micro_usd as f64 / 1_000_000.0;
            let cache_read = m.cache_read_micro_usd as f64 / 1_000_000.0;
            println!(
                "  {:40} events={:>6}  total=${:>10.4}  (in=${:.4} out=${:.4} c_cr=${:.4} c_rd=${:.4})",
                m.model, m.event_count, total, input, output, cache_create, cache_read,
            );
        }
    }

    if !cost.skipped_models.is_empty() {
        println!();
        println!("=== Skipped models (in JSONL but absent from vendored price table) ===");
        for m in &cost.skipped_models {
            println!("  {m}");
        }
        println!(
            "(if any of these are real current models, refresh the vendored price \
             table — see crates/claude_cost/README.md)"
        );
    }

    Ok(())
}
