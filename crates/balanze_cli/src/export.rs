//! Stateless CSV `export` (spec section 10).
//!
//! Re-derives the full usage time-series on every invocation - nothing is
//! persisted (durable history is deferred with the post-1.0 dashboard). Two
//! provenance-segregated sections are written into one CSV stream:
//!
//!   * Claude: one row per `(day, model)` over ALL JSONL history, carrying
//!     token counts and a list-price *leverage* dollar figure
//!     (`jsonl_list_price` / estimate - NOT money billed). For a Pro/Max user
//!     this is subscription leverage, never spend; see `claude_cost` crate docs.
//!   * OpenAI: current-month real billed spend per line item
//!     (`openai_admin_costs` / real).
//!
//! HONESTY DISCIPLINE (AGENTS.md §2.1, §3.3; spec §10/§14): leverage and real
//! billed dollars live in DISTINCT, clearly-named columns and are never summed
//! into one. Mirrors the `--json` provenance contract in `json_output.rs`.
//!
//! TODO(#114): `--since` / `--until` / `--provider` / `--format` filters and a
//! JSON time-series variant are deferred to issue #114. Also deferred there:
//! true per-day OpenAI buckets - the parsed `OpenAiCosts` collapses the daily
//! buckets the Admin Costs API returns into a single month aggregate
//! (`by_line_item` + `start_time`/`end_time`), so per-day OpenAI rows require a
//! buckets-preserving fetch in `openai_client` first.

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use claude_cost::{Cost, PriceTable, compute_cost, load_bundled_prices};
use claude_parser::UsageEvent;
use openai_client::OpenAiCosts;

use crate::cli::ExportArgs;

/// Provenance tag for the Claude leverage column. Matches the `--json`
/// `source` vocabulary (`json_output.rs`) so every surface agrees.
const CLAUDE_PROVENANCE: &str = "jsonl_list_price";
/// Provenance tag for the OpenAI billed column.
const OPENAI_PROVENANCE: &str = "openai_admin_costs";

/// One emitted Claude `(day, model)` row. `leverage_micro_usd` is the
/// list-price estimate; it is NEVER added to any OpenAI billed figure.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaudeRow {
    day: String, // YYYY-MM-DD (UTC)
    model: String,
    event_count: usize,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_creation: u64,
    tokens_cache_read: u64,
    leverage_micro_usd: i64,
}

/// One emitted OpenAI billed row (current-month, per line item).
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenAiRow {
    period_start: String, // YYYY-MM-DD (UTC) - month window start
    period_end: String,   // YYYY-MM-DD (UTC) - month window end (now)
    line_item: String,
    billed_micro_usd: i64,
}

/// Format a UTC timestamp as `YYYY-MM-DD` (the day-bucket key).
fn day_key(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d").to_string()
}

/// Re-derive the full Claude `(day, model)` time-series from ALL events.
///
/// Events are grouped by `(day, model)`; each group runs through the shared
/// pure `compute_cost` so the leverage dollars come from the SAME list-price
/// math as the status/JSON surfaces (DRY). Rows are returned sorted by
/// `(day asc, model asc)` for deterministic, diff-stable CSV output.
///
/// `leverage_micro_usd` is a list-price ESTIMATE (subscription leverage for a
/// Pro/Max user), never billed spend - it is emitted under its own clearly
/// named column and tagged `jsonl_list_price`.
fn claude_rows(events: &[UsageEvent], prices: &PriceTable) -> Vec<ClaudeRow> {
    // Bucket events by (day, model). BTreeMap gives deterministic ordering.
    let mut buckets: BTreeMap<(String, String), Vec<UsageEvent>> = BTreeMap::new();
    for ev in events {
        // Skip empty-model events here: compute_cost routes them to
        // unparsed_event_count and they carry no usable per-model row.
        if ev.model.is_empty() {
            continue;
        }
        buckets
            .entry((day_key(ev.ts), ev.model.clone()))
            .or_default()
            .push(ev.clone());
    }

    let mut rows = Vec::with_capacity(buckets.len());
    for ((day, model), group) in buckets {
        // Token sums are over the raw group (independent of price-table
        // membership) so token columns are complete even for a model the
        // vendored table doesn't yet price.
        let mut tokens_input = 0u64;
        let mut tokens_output = 0u64;
        let mut tokens_cache_creation = 0u64;
        let mut tokens_cache_read = 0u64;
        for ev in &group {
            tokens_input = tokens_input.saturating_add(ev.input_tokens);
            tokens_output = tokens_output.saturating_add(ev.output_tokens);
            tokens_cache_creation =
                tokens_cache_creation.saturating_add(ev.cache_creation_input_tokens);
            tokens_cache_read = tokens_cache_read.saturating_add(ev.cache_read_input_tokens);
        }

        // Leverage via the shared pure cost fn. All events in the group share
        // one model, so per_model is either empty (unknown model -> 0 leverage,
        // tokens still emitted) or a single row.
        let cost: Cost = compute_cost(&group, prices);
        let leverage_micro_usd = cost
            .per_model
            .iter()
            .find(|m| m.model == model)
            .map(|m| m.total_micro_usd)
            .unwrap_or(0);

        rows.push(ClaudeRow {
            day,
            model,
            event_count: group.len(),
            tokens_input,
            tokens_output,
            tokens_cache_creation,
            tokens_cache_read,
            leverage_micro_usd,
        });
    }
    // BTreeMap already yields (day asc, model asc); the explicit type keeps the
    // ordering intent visible at the call site.
    rows
}

/// Project the current-month OpenAI cell into billed rows, one per line item.
///
/// TODO(#114): the Admin Costs API returns DAILY buckets, but the parsed
/// `OpenAiCosts` aggregates them to a month total per line item, so the period
/// here is the whole-month window, not a per-day value. True `(date,
/// line_item, cost)` rows need a buckets-preserving fetch in `openai_client`.
fn openai_rows(costs: &OpenAiCosts) -> Vec<OpenAiRow> {
    let period_start = day_key(costs.start_time);
    let period_end = day_key(costs.end_time);
    costs
        .by_line_item
        .iter()
        .map(|li| OpenAiRow {
            period_start: period_start.clone(),
            period_end: period_end.clone(),
            line_item: li.line_item.clone(),
            billed_micro_usd: li.amount_micro_usd,
        })
        .collect()
}

/// Convert i64 micro-USD to a fixed 6-decimal USD string for CSV. We keep full
/// micro precision (not the 2dp display rounding) so the spreadsheet is
/// lossless; the value is plain text, never summed across the leverage/billed
/// boundary.
fn micro_to_usd_csv(micro: i64) -> String {
    let sign = if micro < 0 { "-" } else { "" };
    let abs = micro.unsigned_abs();
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

/// Write both provenance-segregated sections as one CSV stream.
///
/// Single wide schema with a `section` discriminator (resolves spec §15's
/// "single wide table vs per-provider sections" in favor of one table the
/// provenance-separation rule is trivially satisfied by). Money columns:
/// `leverage_list_price_usd` (Claude estimate) and `billed_usd` (OpenAI real)
/// are SEPARATE columns; a Claude row leaves `billed_usd` empty and vice
/// versa, so the two are never co-located, let alone summed.
fn write_csv<W: Write>(w: &mut W, claude: &[ClaudeRow], openai: &[OpenAiRow]) -> Result<()> {
    let mut wtr = csv::Writer::from_writer(w);
    wtr.write_record([
        "section",
        "provenance",
        "day",
        "period_end",
        "model_or_line_item",
        "event_count",
        "tokens_input",
        "tokens_output",
        "tokens_cache_creation",
        "tokens_cache_read",
        "leverage_list_price_usd",
        "billed_usd",
    ])
    .context("write csv header")?;

    for r in claude {
        wtr.write_record([
            "claude",
            CLAUDE_PROVENANCE,
            r.day.as_str(),
            "", // period_end: claude rows are single-day
            r.model.as_str(),
            r.event_count.to_string().as_str(),
            r.tokens_input.to_string().as_str(),
            r.tokens_output.to_string().as_str(),
            r.tokens_cache_creation.to_string().as_str(),
            r.tokens_cache_read.to_string().as_str(),
            micro_to_usd_csv(r.leverage_micro_usd).as_str(),
            "", // billed_usd: never set on a Claude (leverage) row
        ])
        .context("write claude csv row")?;
    }

    for r in openai {
        wtr.write_record([
            "openai",
            OPENAI_PROVENANCE,
            r.period_start.as_str(),
            r.period_end.as_str(),
            r.line_item.as_str(),
            "", // event_count: not applicable to billed buckets
            "",
            "",
            "",
            "",
            "", // leverage_list_price_usd: never set on a billed row
            micro_to_usd_csv(r.billed_micro_usd).as_str(),
        ])
        .context("write openai csv row")?;
    }

    wtr.flush().context("flush csv writer")?;
    Ok(())
}

/// `balanze-cli export`: stateless CSV of the full usage time-series.
///
/// Re-derives everything live (no persistence). Claude history comes from the
/// same JSONL walk `status` uses; OpenAI is the current-month billed spend. A
/// missing source degrades to an empty section rather than failing the export
/// (an absent provider is normal). A real fetch error propagates as `anyhow`
/// and is classified into an exit code by `main` (PR5).
pub(crate) fn cmd_export(args: &ExportArgs) -> Result<()> {
    // Claude: re-derive ALL events, then the full (day, model) series.
    let (events, _files_scanned) =
        crate::sources::export_load_claude_events().context("loading Claude JSONL for export")?;
    let prices = load_bundled_prices().context("loading bundled price table")?;
    let claude = claude_rows(&events, &prices);

    // OpenAI: current-month billed spend. None (not configured) -> empty
    // section; a fetch error propagates for exit-code classification.
    let openai = match export_fetch_openai()? {
        Some(costs) => openai_rows(&costs),
        None => Vec::new(),
    };

    match &args.output {
        Some(path) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating export file {}", path.display()))?;
            let mut bw = std::io::BufWriter::new(file);
            write_csv(&mut bw, &claude, &openai)?;
        }
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            write_csv(&mut lock, &claude, &openai)?;
        }
    }
    Ok(())
}

/// Run the existing live OpenAI fetch on a local runtime. Mirrors how
/// `run_status` builds a one-shot runtime (`main.rs`), since `main` is a sync
/// `fn` with no top-level tokio runtime.
fn export_fetch_openai() -> Result<Option<OpenAiCosts>> {
    tokio::runtime::Runtime::new()
        .context("building tokio runtime for OpenAI export fetch")?
        .block_on(crate::sources::export_fetch_openai())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use claude_parser::{dedup_events, find_jsonl_files, parse_str};

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    fn load_fixture_events() -> Vec<UsageEvent> {
        let claude_dir = fixture_root().join("claude/projects");
        let files = find_jsonl_files(&claude_dir).expect("fixture JSONL dir exists");
        let mut events = Vec::new();
        for path in &files {
            let content = std::fs::read_to_string(path).expect("fixture readable");
            events.extend(parse_str(&content).expect("fixture parses"));
        }
        dedup_events(&mut events);
        events
    }

    fn fixed_now() -> DateTime<Utc> {
        // Same fixed `now` as integration_4quadrant.rs: 1h after the last
        // fixture event (2026-05-15T10:02Z). For export the value only fixes
        // the OpenAI month-window end string; Claude rows are keyed off each
        // event's own UTC date, so they are now-independent.
        DateTime::parse_from_rfc3339("2026-05-15T11:02:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Hand-built OpenAI cell: two line items in the current month. Real
    /// billed (`openai_admin_costs`). FixtureSources returns Ok(None) for
    /// OpenAI, so the golden builds this explicitly to exercise the section.
    fn sample_openai(now: DateTime<Utc>) -> OpenAiCosts {
        OpenAiCosts {
            start_time: DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: now,
            total_micro_usd: 1_730_000,
            by_line_item: vec![
                openai_client::LineItemCost {
                    line_item: "gpt-5".to_string(),
                    amount_micro_usd: 1_650_000,
                },
                openai_client::LineItemCost {
                    line_item: "o1-mini".to_string(),
                    amount_micro_usd: 80_000,
                },
            ],
            truncated: false,
            fetched_at: now,
        }
    }

    #[test]
    fn claude_rows_are_keyed_by_day_and_model_with_leverage() {
        let events = load_fixture_events();
        let prices = load_bundled_prices().expect("bundled prices");
        let rows = claude_rows(&events, &prices);

        // Fixture: 3 dedup'd events all on 2026-05-15 - sonnet-4-6 x2,
        // haiku-4-5 x1 -> exactly 2 (day, model) rows.
        assert_eq!(rows.len(), 2, "got: {rows:?}");
        for r in &rows {
            assert_eq!(r.day, "2026-05-15");
        }
        let sonnet = rows
            .iter()
            .find(|r| r.model == "claude-sonnet-4-6")
            .expect("sonnet row present");
        assert_eq!(sonnet.event_count, 2);
        // Leverage is a positive list-price estimate, never zero for priced
        // sonnet events, and is the claude_cost figure - not a billed number.
        assert!(sonnet.leverage_micro_usd > 0);

        let haiku = rows
            .iter()
            .find(|r| r.model == "claude-haiku-4-5")
            .expect("haiku row present");
        assert_eq!(haiku.event_count, 1);

        // Rows are deterministically ordered (day asc, then model asc).
        assert_eq!(rows[0].model, "claude-haiku-4-5");
        assert_eq!(rows[1].model, "claude-sonnet-4-6");
    }

    #[test]
    fn openai_rows_carry_real_billed_per_line_item() {
        let now = fixed_now();
        let rows = openai_rows(&sample_openai(now));
        assert_eq!(rows.len(), 2);
        // Order mirrors by_line_item order (the parsed cell is sorted desc by
        // amount upstream).
        assert_eq!(rows[0].line_item, "gpt-5");
        assert_eq!(rows[0].billed_micro_usd, 1_650_000);
        assert_eq!(rows[1].line_item, "o1-mini");
        assert_eq!(rows[0].period_start, "2026-05-01");
        assert_eq!(rows[0].period_end, "2026-05-15");
    }

    #[test]
    fn csv_keeps_leverage_and_billed_in_distinct_columns_never_summed() {
        let events = load_fixture_events();
        let prices = load_bundled_prices().expect("bundled prices");
        let now = fixed_now();
        let mut buf: Vec<u8> = Vec::new();
        write_csv(
            &mut buf,
            &claude_rows(&events, &prices),
            &openai_rows(&sample_openai(now)),
        )
        .expect("write_csv ok");
        let out = String::from_utf8(buf).expect("utf8");

        // Provenance segregation: the leverage and billed columns are
        // structurally distinct headers, mirroring the --json contract.
        assert!(
            out.contains("leverage_list_price_usd"),
            "leverage column header missing:\n{out}"
        );
        assert!(
            out.contains("billed_usd"),
            "billed column header missing:\n{out}"
        );
        assert_ne!(
            "leverage_list_price_usd", "billed_usd",
            "the two money columns must be different names"
        );
        // Provenance tags present and distinct so a leverage row can never be
        // misread as billed spend.
        assert!(
            out.contains(CLAUDE_PROVENANCE),
            "missing jsonl_list_price tag"
        );
        assert!(
            out.contains(OPENAI_PROVENANCE),
            "missing openai_admin_costs tag"
        );
        // No single column header conflates the two (e.g. a "total_usd" that
        // would invite summing leverage + billed).
        assert!(
            !out.contains("total_usd"),
            "export must not emit a column that sums leverage + billed:\n{out}"
        );

        // Golden Claude rows: stable section+provenance+day+model content. The
        // empty field between `day` and the model is the unused `period_end`
        // column (claude rows are single-day), hence the doubled comma.
        assert!(
            out.contains("claude,jsonl_list_price,2026-05-15,,claude-haiku-4-5,"),
            "haiku row:\n{out}"
        );
        assert!(
            out.contains("claude,jsonl_list_price,2026-05-15,,claude-sonnet-4-6,"),
            "sonnet row:\n{out}"
        );
    }

    #[test]
    fn micro_to_usd_csv_formats_full_precision_and_sign() {
        assert_eq!(micro_to_usd_csv(0), "0.000000");
        assert_eq!(micro_to_usd_csv(1_650_000), "1.650000");
        assert_eq!(micro_to_usd_csv(80_000), "0.080000");
        // Negative (e.g. a credit) keeps the sign and stays unsummed text.
        assert_eq!(micro_to_usd_csv(-2_500_000), "-2.500000");
    }
}
