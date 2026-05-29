//! Criterion bench for `IncrementalParser::read_incremental` on a typical
//! Claude Code "appended 100 lines" tick.
//!
//! **Design target: < 200µs per 100 new lines on release-mode x86_64.**
//! Rationale: the watcher fires `read_incremental` on every JSONL notify
//! event during an active Claude Code session (AGENTS.md §3.1: "JSONL:
//! local file I/O, no rate limit, but read incrementally via per-file
//! byte cursor"). At the upper end of a chatty session that's ~10
//! events/sec; staying under 200µs per tick keeps the parser well below
//! 1% CPU on the hot path.
//!
//! **Measured baseline (track_e_initial): ~7 ms** on Windows 11 dev box
//! against a tempfile under the system temp dir. The bulk is Win32
//! `CreateFile` + `GetFileInformationByHandle` overhead on the temp
//! directory, NOT the parser's hot loop (a 25 KB read + 100 JSON parses
//! is well under 1 ms in isolation). Linux/macOS measurements are
//! expected to be substantially closer to the design target; once the
//! v0.3 file watcher reads real `~/.claude/projects/**/*.jsonl` (which
//! lives off the system temp dir on a non-FAT volume) this should
//! converge toward the budget on Windows too.
//!
//! What the committed baseline IS good for either way: regression
//! detection. A future change that triples this number gets caught even
//! though the absolute figure is higher than design.
//!
//! Baseline workflow. `cargo bench -p claude_parser -- --save-baseline
//! track_e_initial` writes Criterion's output to
//! `target/criterion/incremental_parser_100_new_lines/track_e_initial/estimates.json`.
//! The committed `crates/claude_parser/benches/baseline.json` is a
//! **manual copy** of that file — a reference snapshot at Track E ship
//! time. Criterion does NOT auto-consume the committed file; on a fresh
//! checkout, `cargo bench -- --baseline track_e_initial` finds nothing
//! because `target/criterion/` is empty. To compare against the committed
//! snapshot, copy `crates/claude_parser/benches/baseline.json` into
//! `target/criterion/incremental_parser_100_new_lines/track_e_initial/estimates.json`
//! first. To refresh: run `--save-baseline track_e_initial` and copy the
//! new `estimates.json` back over `benches/baseline.json`.

use std::io::Write;

use claude_parser::IncrementalParser;
use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use tempfile::NamedTempFile;

/// Realistic assistant-line shape. Token counts vary so the parser's
/// number deserialization isn't a constant-fold candidate.
fn jsonl_line(i: usize) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"2026-01-01T00:{:02}:{:02}.000Z","message":{{"id":"msg_{i}","model":"claude-sonnet-4-6","usage":{{"input_tokens":{},"output_tokens":{},"cache_creation_input_tokens":50,"cache_read_input_tokens":100}}}},"requestId":"req_{i}"}}{}"#,
        (i / 60) % 60,
        i % 60,
        1000 + (i % 500),
        500 + (i % 300),
        "\n"
    )
}

fn bench_incremental_100_new_lines(c: &mut Criterion) {
    c.bench_function("incremental_parser_100_new_lines", |b| {
        // `iter_batched` recreates the tempfile + warms the cursor for
        // every iter; `iter_with_setup` is deprecated and grows
        // unbounded. The benched closure is only the appended-100-line
        // read, which is what the watcher does in steady state.
        b.iter_batched(
            || {
                let mut file = NamedTempFile::new().expect("create tempfile");
                // 500 warm-up lines: large enough that the cursor is well
                // away from byte 0, small enough that initial parse cost
                // doesn't dwarf the bench setup time across iters.
                for i in 0..500 {
                    file.write_all(jsonl_line(i).as_bytes()).unwrap();
                }
                file.flush().unwrap();

                let mut parser = IncrementalParser::new();
                parser
                    .read_incremental(file.path())
                    .expect("initial parse seeds the cursor");

                // Append exactly 100 new lines — the unit the budget is
                // expressed in.
                for i in 500..600 {
                    file.write_all(jsonl_line(i).as_bytes()).unwrap();
                }
                file.flush().unwrap();

                (parser, file)
            },
            |(mut parser, file)| {
                let events = parser
                    .read_incremental(black_box(file.path()))
                    .expect("incremental read of appended lines");
                black_box(events);
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, bench_incremental_100_new_lines);
criterion_main!(benches);
