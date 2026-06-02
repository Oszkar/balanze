//! Criterion bench for [`summarize_window`] over 10k events on a 5h slice.
//!
//! **Budget: < 1ms on release-mode x86_64** for 10k events with a 5h main
//! window + 30m burn window. Rationale: window math runs on every
//! `StateMsg::Update` from the JSONL source (notify event OR the 60s
//! safety poll); a slow summary stalls the coordinator and the
//! `(ColorBucket, title_text)` dedup (AGENTS.md §3.1) that follows.
//!
//! Baseline workflow. `cargo bench -p window -- --save-baseline
//! committed` writes Criterion's output to
//! `target/criterion/summarize_window_10k_5h/committed/estimates.json`.
//! The committed `crates/window/benches/baseline.json` is a **manual copy**
//! of that file — a reference snapshot. Criterion
//! does NOT auto-consume the committed file; on a fresh checkout,
//! `cargo bench -- --baseline committed` finds nothing because
//! `target/criterion/` is empty. To compare against the committed
//! snapshot, copy `crates/window/benches/baseline.json` into
//! `target/criterion/summarize_window_10k_5h/committed/estimates.json`
//! first. To refresh: run `--save-baseline committed` and copy the
//! new `estimates.json` back over `benches/baseline.json`.

use chrono::{Duration, TimeZone, Utc};
use claude_parser::{AccountType, DataSource, Provider, UsageEvent};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use window::summarize_window;

/// 10k events spread evenly across the 5-hour window so the inner-loop
/// `within_upper`/`burn_window_start` filters both trigger realistic
/// branch-prediction patterns. Two distinct models so the `by_model` map
/// insertion isn't degenerate.
fn synthetic_events(n: usize, base: chrono::DateTime<Utc>) -> Vec<UsageEvent> {
    let models = ["claude-sonnet-4-6", "claude-opus-4-7"];
    let span_ms = (Duration::hours(5).num_milliseconds()) as usize;
    let step_ms = (span_ms / n.max(1)) as i64;
    (0..n)
        .map(|i| UsageEvent {
            ts: base + Duration::milliseconds(step_ms * i as i64),
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: models[i % models.len()].to_string(),
            input_tokens: 500,
            output_tokens: 200,
            cache_creation_input_tokens: 50,
            cache_read_input_tokens: 100,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: None,
            request_id: None,
        })
        .collect()
}

fn bench_summarize_window(c: &mut Criterion) {
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 18, 0, 0).unwrap();
    // Anchor `now` at the END of the 5h slice so every event lands inside
    // the cap window — the worst-case path for the inner loop.
    let base = now - Duration::hours(5);
    let events = synthetic_events(10_000, base);
    let window = Duration::hours(5);
    let burn = Duration::minutes(30);

    c.bench_function("summarize_window_10k_5h", |b| {
        b.iter(|| {
            let summary = summarize_window(
                black_box(&events),
                black_box(now),
                black_box(window),
                black_box(burn),
                black_box(3),
                black_box(None),
            );
            black_box(summary);
        });
    });
}

criterion_group!(benches, bench_summarize_window);
criterion_main!(benches);
