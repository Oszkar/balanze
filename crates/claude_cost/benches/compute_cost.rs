//! Criterion bench for [`compute_cost`] over 10k synthetic events.
//!
//! **Budget: < 5ms on release-mode x86_64** for 10k events on the bundled
//! price table. Rationale: this runs inside `state_coordinator`'s merge path
//! on every JSONL change. A modal 5h Claude session yields O(10k) events;
//! the merge must finish well inside the 30s tray-repaint cadence
//! (AGENTS.md §3.1) with budget to spare for the other sources.
//!
//! Baseline workflow. `cargo bench -p claude_cost -- --save-baseline
//! track_e_initial` writes Criterion's output to
//! `target/criterion/compute_cost_10k_events/track_e_initial/estimates.json`.
//! The committed `crates/claude_cost/benches/baseline.json` is a **manual
//! copy** of that file — a reference snapshot of what the bench looked like
//! at Track E ship time. Criterion does NOT auto-consume the committed
//! file; on a fresh checkout, `cargo bench -- --baseline track_e_initial`
//! finds nothing because `target/criterion/` is empty. To compare against
//! the committed snapshot, copy
//! `crates/claude_cost/benches/baseline.json` into
//! `target/criterion/compute_cost_10k_events/track_e_initial/estimates.json`
//! first, then run with `--baseline track_e_initial`. To refresh the
//! committed snapshot, run with `--save-baseline track_e_initial` and copy
//! the new `estimates.json` back over `benches/baseline.json`.

use chrono::{TimeZone, Utc};
use claude_cost::{compute_cost, load_bundled_prices};
use claude_parser::{AccountType, DataSource, Provider, UsageEvent};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Build a deterministic slice of N synthetic events spread across three
/// real model names from the bundled price table. Mixing models exercises
/// the per-model `by_model` BTreeMap insertion path (the realistic case);
/// a single-model run would bottom out in a much cheaper code path.
fn synthetic_events(n: usize) -> Vec<UsageEvent> {
    let base = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let models = ["claude-sonnet-4-6", "claude-opus-4-7", "claude-haiku-4-5"];
    (0..n)
        .map(|i| UsageEvent {
            ts: base + chrono::Duration::seconds(i as i64),
            provider: Provider::Claude,
            account_type: AccountType::Api,
            model: models[i % models.len()].to_string(),
            input_tokens: 1000 + (i as u64 % 500),
            output_tokens: 500 + (i as u64 % 300),
            cache_creation_input_tokens: 100,
            cache_read_input_tokens: 200,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: Some(format!("msg_{i}")),
            request_id: Some(format!("req_{i}")),
        })
        .collect()
}

fn bench_compute_cost(c: &mut Criterion) {
    let events = synthetic_events(10_000);
    let prices = load_bundled_prices().expect("bundled prices must load");

    c.bench_function("compute_cost_10k_events", |b| {
        b.iter(|| {
            let cost = compute_cost(black_box(&events), black_box(&prices));
            black_box(cost);
        });
    });
}

criterion_group!(benches, bench_compute_cost);
criterion_main!(benches);
