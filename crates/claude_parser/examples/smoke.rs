//! Smoke test against the user's real ~/.claude/projects/ data.
//!
//! Run with:
//!   cargo run --release --example smoke -p claude_parser
//!
//! Prints a summary (files, events, total tokens) and a per-model breakdown.
//! Parse errors are logged but don't abort — we want to see whether the
//! parser tolerates the real-world JSONL distribution.

use std::collections::HashMap;
use std::fs;

use claude_parser::{find_jsonl_files, parse_str};

fn main() -> anyhow::Result<()> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("could not resolve user's base directories"))?;
    let claude_dir = base_dirs.home_dir().join(".claude").join("projects");
    println!("Scanning {}", claude_dir.display());

    let files = find_jsonl_files(&claude_dir)?;
    println!("Found {} JSONL files", files.len());

    let mut total_events = 0usize;
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache_create = 0u64;
    let mut total_cache_read = 0u64;
    let mut by_model: HashMap<String, (usize, u64)> = HashMap::new();
    let mut parse_error_files = 0usize;
    let mut earliest: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;

    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("read failed for {}: {e}", path.display());
                continue;
            }
        };
        match parse_str(&content) {
            Ok(events) => {
                for ev in events {
                    total_events += 1;
                    total_input = total_input.saturating_add(ev.input_tokens);
                    total_output = total_output.saturating_add(ev.output_tokens);
                    total_cache_create =
                        total_cache_create.saturating_add(ev.cache_creation_input_tokens);
                    total_cache_read = total_cache_read.saturating_add(ev.cache_read_input_tokens);
                    let entry = by_model.entry(ev.model.clone()).or_insert((0, 0));
                    entry.0 += 1;
                    entry.1 = entry.1.saturating_add(ev.total_tokens());
                    earliest = Some(earliest.map_or(ev.ts, |e| e.min(ev.ts)));
                    latest = Some(latest.map_or(ev.ts, |l| l.max(ev.ts)));
                }
            }
            Err(e) => {
                eprintln!("parse error in {}: {e}", path.display());
                parse_error_files += 1;
            }
        }
    }

    println!();
    println!("=== Summary ===");
    println!("Files:                  {}", files.len());
    println!("Parse-error files:      {parse_error_files}");
    println!("Events:                 {total_events}");
    println!("Input tokens:           {total_input}");
    println!("Output tokens:          {total_output}");
    println!("Cache creation tokens:  {total_cache_create}");
    println!("Cache read tokens:      {total_cache_read}");
    println!(
        "Total billed tokens:    {}",
        total_input + total_output + total_cache_create + total_cache_read
    );
    if let (Some(e), Some(l)) = (earliest, latest) {
        println!(
            "Date range:             {} to {}",
            e.to_rfc3339(),
            l.to_rfc3339()
        );
    }

    let mut models: Vec<_> = by_model.into_iter().collect();
    models.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    if !models.is_empty() {
        println!();
        println!("By model (sorted by total tokens):");
        for (model, (events, tokens)) in models {
            let display = if model.is_empty() {
                "(unknown)".to_string()
            } else {
                model
            };
            println!(
                "  {:40} events={:>6}  tokens={:>14}",
                display, events, tokens
            );
        }
    }
    Ok(())
}
