//! The status renderers: the compact 4-quadrant matrix (default output) and
//! the per-source `--sections` detail view. Both render the same `Snapshot`;
//! the writer-driven `write_*` siblings exist for golden-string tests.

use std::io::{self, Write};

use state_coordinator::Snapshot;

use crate::format::{
    fmt_int, format_codex_age, format_codex_window, micro_usd_to_display_dollars, pretty_duration,
    short_cadence,
};

pub(crate) fn print_sections(snapshot: &Snapshot, verbose: bool) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    write_sections(snapshot, verbose, &mut lock).or_else(|e| {
        if e.kind() == io::ErrorKind::BrokenPipe {
            Ok(())
        } else {
            Err(e)
        }
    })
}

/// Writer-driven sibling of [`print_sections`]. Same content, parameterized
/// over the sink for golden-string tests of the per-source detail view.
fn write_sections<W: Write>(snapshot: &Snapshot, verbose: bool, w: &mut W) -> io::Result<()> {
    writeln!(w, "=== Balanze Status ===")?;
    writeln!(
        w,
        "fetched: {}",
        snapshot.fetched_at.format("%Y-%m-%d %H:%M:%S UTC")
    )?;
    writeln!(w)?;

    // Claude OAuth (cadence bars)
    if let Some(oauth) = &snapshot.claude_oauth {
        writeln!(
            w,
            "subscription: {} ({})",
            oauth.subscription_type.as_deref().unwrap_or("?"),
            oauth.rate_limit_tier.as_deref().unwrap_or("?"),
        )?;
        // org_uuid identifies the user's Anthropic consumer org. Useful for
        // bug reports but doxes the account when pasted publicly, so it's
        // gated behind --verbose / -v.
        if verbose {
            if let Some(uuid) = &oauth.org_uuid {
                writeln!(w, "org uuid:     {uuid}")?;
            }
        }
        writeln!(w)?;
        writeln!(w, "CADENCE BARS (from Anthropic OAuth):")?;
        if oauth.cadences.is_empty() {
            writeln!(w, "  (none reported)")?;
        }
        for cad in &oauth.cadences {
            let resets_in = cad.resets_at.signed_duration_since(snapshot.fetched_at);
            writeln!(
                w,
                "  {:32}  {:>6.2}%   resets in {}",
                cad.display_label,
                cad.utilization_percent,
                pretty_duration(resets_in)
            )?;
        }
        // Extra-usage = pay-as-you-go overage. The raw ints are cents; this is
        // the claude.ai "Extra usage" meter - REAL billed money, distinct from
        // the estimated API-rate figure below. Only meaningful when the user
        // enabled it.
        if let Some(eu) = &oauth.extra_usage {
            if eu.is_enabled {
                writeln!(w)?;
                writeln!(
                    w,
                    "EXTRA USAGE (pay-as-you-go overage - REAL billed spend, from Anthropic OAuth):"
                )?;
                writeln!(
                    w,
                    "  Spent this cycle:  {} of {} ({:.1}%)",
                    micro_usd_to_display_dollars(eu.used_credits_micro_usd),
                    micro_usd_to_display_dollars(eu.monthly_limit_micro_usd),
                    eu.utilization_percent
                )?;
                writeln!(
                    w,
                    "  Real money billed beyond your subscription - NOT the estimate below."
                )?;
            } else {
                writeln!(w)?;
                writeln!(
                    w,
                    "EXTRA USAGE: disabled (no pay-as-you-go overage configured)"
                )?;
            }
        }
    } else if let Some(err) = &snapshot.claude_oauth_error {
        writeln!(w, "CADENCE BARS: unavailable - {err}")?;
    }

    // OpenAI monthly costs (from Admin API)
    writeln!(w)?;
    if let Some(costs) = &snapshot.openai {
        writeln!(
            w,
            "OPENAI SPEND ({} – {}):",
            costs.start_time.format("%Y-%m-%d"),
            costs.end_time.format("%Y-%m-%d"),
        )?;
        let suffix = if costs.truncated {
            "  (partial; more pages available)"
        } else {
            ""
        };
        writeln!(
            w,
            "  Total: {}{suffix}",
            micro_usd_to_display_dollars(costs.total_micro_usd)
        )?;
        if !costs.by_line_item.is_empty() {
            writeln!(w)?;
            writeln!(w, "  By line item:")?;
            for item in costs.by_line_item.iter().take(10) {
                writeln!(
                    w,
                    "    {:36}  ${:>10.4}",
                    item.line_item,
                    item.amount_micro_usd as f64 / 1_000_000.0
                )?;
            }
            if costs.by_line_item.len() > 10 {
                writeln!(w, "    ... ({} more)", costs.by_line_item.len() - 10)?;
            }
        }
    } else if let Some(err) = &snapshot.openai_error {
        writeln!(w, "OPENAI SPEND: unavailable - {err}")?;
    } else {
        writeln!(w, "OPENAI SPEND: not configured")?;
        writeln!(
            w,
            "  Run `balanze-cli set-openai-key` to store an `sk-admin-...` admin key, or set"
        )?;
        writeln!(w, "  the BALANZE_OPENAI_KEY env var.")?;
        writeln!(
            w,
            "  Create an admin key at https://platform.openai.com/settings/organization/admin-keys"
        )?;
    }

    // Claude Code JSONL activity
    writeln!(w)?;
    if let Some(jsonl) = &snapshot.claude_jsonl {
        writeln!(w, "CLAUDE CODE ACTIVITY (last 5h, from local JSONL):")?;
        writeln!(w, "  files scanned:     {}", jsonl.files_scanned)?;
        writeln!(
            w,
            "  events in window:  {}",
            jsonl.window.total_events_in_window
        )?;
        writeln!(
            w,
            "  tokens in window:  {}",
            fmt_int(jsonl.window.total_tokens_in_window)
        )?;
        match jsonl.window.recent_burn_tokens_per_min {
            Some(rate) => writeln!(
                w,
                "  recent burn:       ~{} tokens/min (last 30 min)",
                fmt_int(rate as u64)
            )?,
            None => writeln!(w, "  recent burn:       (too few events in last 30 min)")?,
        }
        if !jsonl.window.by_model.is_empty() {
            writeln!(w)?;
            writeln!(w, "  By model:")?;
            for m in &jsonl.window.by_model {
                writeln!(
                    w,
                    "    {:36}  events: {:>4}  tokens: {:>14}",
                    m.model,
                    m.events,
                    fmt_int(m.total_tokens)
                )?;
            }
        }
    } else if let Some(err) = &snapshot.claude_jsonl_error {
        writeln!(w, "CLAUDE CODE ACTIVITY: unavailable - {err}")?;
    }

    // Anthropic API cost (estimated, JSONL-derived via claude_cost).
    writeln!(w)?;
    if let Some(cost) = &snapshot.anthropic_api_cost {
        writeln!(
            w,
            "ANTHROPIC API COST - ESTIMATE ONLY (JSONL × LiteLLM list-price @ {} / {}):",
            claude_cost::PRICE_TABLE_COMMIT,
            claude_cost::PRICE_TABLE_DATE,
        )?;
        writeln!(
            w,
            "  Est. list-price:   {} - subscription leverage, NOT money billed",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        )?;
        writeln!(
            w,
            "  (Real out-of-pocket spend is in the EXTRA USAGE block, shown when extra usage is enabled.)"
        )?;
        writeln!(w, "  Events processed:  {}", cost.total_event_count)?;
        if cost.unparsed_event_count > 0 {
            writeln!(
                w,
                "  Unparsed events:   {} (JSONL line lacked model field)",
                cost.unparsed_event_count
            )?;
        }
        if !cost.per_model.is_empty() {
            writeln!(w)?;
            writeln!(w, "  By model (top 10 by spend):")?;
            for m in cost.per_model.iter().take(10) {
                writeln!(
                    w,
                    "    {:36}  events: {:>4}  {}",
                    m.model,
                    m.event_count,
                    micro_usd_to_display_dollars(m.total_micro_usd)
                )?;
            }
            if cost.per_model.len() > 10 {
                writeln!(w, "    ... ({} more)", cost.per_model.len() - 10)?;
            }
        }
        if !cost.skipped_models.is_empty() {
            writeln!(w)?;
            writeln!(
                w,
                "  Skipped models (in JSONL but absent from price table):"
            )?;
            for name in &cost.skipped_models {
                writeln!(w, "    {name}")?;
            }
        }
    } else if let Some(err) = &snapshot.anthropic_api_cost_error {
        writeln!(w, "ANTHROPIC API COST: unavailable - {err}")?;
    } else if snapshot.claude_jsonl_error.is_some() {
        // No separate cost error; the underlying JSONL load failed
        // (already reported above).
        writeln!(
            w,
            "ANTHROPIC API COST: unavailable - JSONL load failed (see above)."
        )?;
    }

    // OpenAI Codex CLI rate-limit snapshot (from codex_local).
    writeln!(w)?;
    if let Some(q) = &snapshot.codex_quota {
        // observed_at is the Codex CLI's own timestamp on the rate-limit
        // event. The "age" here is `fetched_at - observed_at` - how stale
        // the snapshot is relative to right-now. `codex_local::walker`
        // always returns the newest-mtime file regardless of how old it
        // is (see its docs), so a user who hasn't run Codex in a week
        // still sees data - surfacing the age lets them judge for
        // themselves whether to trust it.
        let age_tag = format_codex_age(q.observed_at, snapshot.fetched_at)
            .map(|s| format!(", {s} old"))
            .unwrap_or_default();
        writeln!(
            w,
            "OPENAI CODEX QUOTA (plan: {}, observed {}{age_tag}):",
            q.plan_type,
            q.observed_at.format("%Y-%m-%d %H:%M:%S UTC"),
        )?;
        let resets_in = q
            .primary
            .resets_at
            .signed_duration_since(snapshot.fetched_at);
        writeln!(
            w,
            "  Primary window:    {:.2}% of {} minutes  (resets in {})",
            q.primary.used_percent,
            q.primary.window_duration_minutes,
            pretty_duration(resets_in),
        )?;
        if let Some(secondary) = &q.secondary {
            let s_resets = secondary
                .resets_at
                .signed_duration_since(snapshot.fetched_at);
            writeln!(
                w,
                "  Secondary window:  {:.2}% of {} minutes  (resets in {})",
                secondary.used_percent,
                secondary.window_duration_minutes,
                pretty_duration(s_resets),
            )?;
        }
        if q.rate_limit_reached {
            writeln!(
                w,
                "  ⚠  Rate-limit reached - Codex CLI is currently throttling requests."
            )?;
        }
        if verbose {
            writeln!(w, "  Session ID:        {}", q.session_id)?;
        }
    } else if let Some(err) = &snapshot.codex_quota_error {
        writeln!(w, "OPENAI CODEX QUOTA: unavailable - {err}")?;
    } else {
        writeln!(
            w,
            "OPENAI CODEX QUOTA: not configured (Codex CLI not installed, or no sessions yet)."
        )?;
    }
    Ok(())
}

/// Compact 4-quadrant matrix renderer - the default `balanze-cli` output.
///
/// One screen, no scrolling. The layout maps directly onto the design
/// doc's 4-quadrant matrix: rows are providers (Anthropic, OpenAI),
/// columns are cells (Quota %, API $). Cell content shows ✓ / ○ / ✗
/// plus a one-line summary. See `print_sections` for per-source depth.
pub(crate) fn print_compact(snapshot: &Snapshot) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    write_compact(snapshot, &mut lock).or_else(|e| {
        if e.kind() == io::ErrorKind::BrokenPipe {
            Ok(())
        } else {
            Err(e)
        }
    })
}

/// Writer-driven sibling of [`print_compact`]. Same content, parameterized
/// over the sink so tests can capture the rendered bytes against the
/// four-tier label discipline (estimate / real overage / Claude session
/// estimate / server quota %) without piping stdout.
pub(crate) fn write_compact<W: Write>(snapshot: &Snapshot, w: &mut W) -> io::Result<()> {
    writeln!(
        w,
        "=== Balanze status ({}) ===",
        snapshot.fetched_at.format("%Y-%m-%d %H:%M:%S UTC")
    )?;
    writeln!(w)?;

    let anth_quota = compact_anthropic_quota(snapshot);
    let anth_cost = compact_anthropic_cost(snapshot);
    let openai_quota = compact_codex_quota(snapshot);
    let openai_cost = compact_openai_cost(snapshot);

    writeln!(
        w,
        "                    {:38}  API $ (real billed)",
        "Quota %"
    )?;
    writeln!(w, "Anthropic           {anth_quota:38}  {anth_cost}")?;
    writeln!(w, "OpenAI              {openai_quota:38}  {openai_cost}")?;
    writeln!(w)?;

    if let Some(pace) = compact_pace_line(snapshot) {
        writeln!(w, "{pace}")?;
    }
    if let Some(lev) = compact_subscription_leverage(snapshot) {
        writeln!(w, "{lev}")?;
    }
    writeln!(w)?;

    // The matrix holds measured reality only - server-reported quota % and
    // real billed $. The list-price estimate is the separate "Subscription
    // leverage" line above, never a matrix cell, so a ~$4,000 estimate is
    // never mistaken for ~$4,000 of real spend.
    writeln!(
        w,
        "Quota % = live server-reported utilization. API $ = real billed spend"
    )?;
    writeln!(
        w,
        "only: Anthropic = pay-as-you-go overage (n/a unless enabled); OpenAI ="
    )?;
    writeln!(w, "Admin Costs API. 'Subscription leverage' is a separate")?;
    writeln!(w, "list-price estimate, never charged.")?;
    writeln!(w)?;
    writeln!(
        w,
        "Run `balanze-cli --sections` for per-source detail, or `balanze-cli --json` for machine-readable output."
    )
}

fn compact_anthropic_quota(s: &Snapshot) -> String {
    match (&s.claude_oauth, &s.claude_oauth_error) {
        (Some(oauth), _) => {
            if oauth.cadences.is_empty() {
                "✓ ready (no cadence bars reported)".to_string()
            } else {
                // First two cadences. `anthropic_oauth` pre-sorts by
                // cadence_sort_key (five_hour=0, seven_day=1, ...), so the
                // common case is "5h + 7d". {:.1} (not {:.0}) so a
                // genuine 0.4% doesn't render as "0%" - indistinguishable
                // from the no-usage case.
                let parts: Vec<String> = oauth
                    .cadences
                    .iter()
                    .take(2)
                    .map(|c| format!("{:.1}% {}", c.utilization_percent, short_cadence(&c.key)))
                    .collect();
                format!("✓ {} (oauth)", parts.join(", "))
            }
        }
        (None, Some(_)) => "✗ oauth fetch failed".to_string(),
        (None, None) => "○ not configured".to_string(),
    }
}

fn compact_anthropic_cost(s: &Snapshot) -> String {
    // Measured-only matrix cell: real billed money or nothing. The list-price
    // estimate is NOT here - it renders on the separate "Subscription leverage"
    // line (see compact_subscription_leverage).
    match s
        .claude_oauth
        .as_ref()
        .and_then(|o| o.extra_usage.as_ref())
        .filter(|eu| eu.is_enabled)
    {
        Some(eu) => format!(
            "{}/{} overage (real)",
            micro_usd_to_display_dollars(eu.used_credits_micro_usd),
            micro_usd_to_display_dollars(eu.monthly_limit_micro_usd)
        ),
        None => "- not available".to_string(),
    }
}

/// The JSONL list-price estimate, rendered as a clearly-secondary insight
/// OUTSIDE the matrix - what the local Claude Code usage would cost at API
/// list prices. Subscription leverage, never billed. Also surfaces JSONL /
/// cost-synthesis failures here: the measured-only matrix cell can't show them
/// (it's real-billed-$ only), so without this line those errors would be
/// invisible in the compact view. `None` only when there's genuinely no data
/// and no error.
fn compact_subscription_leverage(s: &Snapshot) -> Option<String> {
    if let Some(cost) = &s.anthropic_api_cost {
        if cost.total_event_count > 0 {
            return Some(format!(
                "Subscription leverage: ~{} of Claude Code usage at API list prices (leverage - NOT billed)",
                micro_usd_to_display_dollars(cost.total_micro_usd)
            ));
        }
    }
    if s.anthropic_api_cost_error.is_some() {
        return Some("Subscription leverage: ✗ cost synthesis failed".to_string());
    }
    if s.claude_jsonl_error.is_some() {
        return Some("Subscription leverage: ✗ jsonl load failed".to_string());
    }
    None
}

/// Per-window pace line: used % vs elapsed % of the window, plus the ratio.
/// `None` when no pace data is present.
fn compact_pace_line(s: &Snapshot) -> Option<String> {
    if s.pace.is_empty() {
        return None;
    }
    let parts: Vec<String> = s
        .pace
        .iter()
        .map(|p| {
            let ratio = match p.ratio {
                Some(r) => format!("{r:.1}×"),
                None => "-".to_string(),
            };
            format!(
                "{} {:.0}% used / {:.0}% elapsed ({ratio})",
                short_cadence(&p.key),
                p.used_fraction * 100.0,
                p.elapsed_fraction * 100.0,
            )
        })
        .collect();
    Some(format!("Pace: {}", parts.join(";  ")))
}

fn compact_codex_quota(s: &Snapshot) -> String {
    match (&s.codex_quota, &s.codex_quota_error) {
        (Some(q), _) => {
            let window = format_codex_window(q.primary.window_duration_minutes);
            // Honesty: a rollout whose primary window has already reset is
            // stale - the used% it carries describes an elapsed window. Degrade
            // the ✓ to a ⚠ + "stale" marker rather than show a confident figure
            // behind a green check.
            let expired = s.fetched_at > q.primary.resets_at;
            // Append snapshot age when meaningfully stale (≥1 min). The
            // walker returns the newest-mtime rollout file regardless of
            // age, so a 7-day-old session and a 2-min-old session look
            // identical without this tag - see `format_codex_age` doc.
            let age_tag = format_codex_age(q.observed_at, s.fetched_at)
                .map(|s| format!(", {s} old"))
                .unwrap_or_default();
            let (marker, stale_tag) = if expired {
                ("⚠", ", stale")
            } else {
                ("✓", "")
            };
            // {:.1} for the same reason as the anthropic quota cell - a
            // genuine 0.4% must not collapse to "0%".
            format!(
                "{marker} {:.1}% {window} (codex {}{stale_tag}{age_tag})",
                q.primary.used_percent, q.plan_type
            )
        }
        (None, Some(_)) => "✗ codex_local error".to_string(),
        (None, None) => "○ not configured (codex)".to_string(),
    }
}

fn compact_openai_cost(s: &Snapshot) -> String {
    match (&s.openai, &s.openai_error) {
        (Some(costs), _) => format!(
            "{} (admin costs)",
            micro_usd_to_display_dollars(costs.total_micro_usd)
        ),
        (None, Some(_)) => "✗ admin costs fetch failed".to_string(),
        (None, None) => "○ not configured (run `balanze-cli setup`)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot, ExtraUsage};
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use claude_cost::{Cost, ModelCost};
    use codex_local::{CodexQuotaSnapshot, RateLimitWindow};
    use openai_client::{LineItemCost, OpenAiCosts};
    use state_coordinator::JsonlSnapshot;
    use window::{ByModel, WindowSummary};

    fn t(year: i32, month: u32, day: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, h, m, 0).unwrap()
    }

    // -----------------------------------------------------------------------
    // Label-discipline goldens for the compact + sections views. These pin
    // the four-tier confidence vocabulary the product depends on:
    //   1. ESTIMATE (JSONL × LiteLLM list price - subscription leverage)
    //   2. REAL pay-as-you-go overage (Anthropic extra_usage block)
    //   3. Server quota % (OAuth cadences + Codex rate-limit)
    //   4. Real billed spend (OpenAI Admin Costs)
    // A future refactor that drops "estimate" / "NOT billed" / "REAL" /
    // "overage" qualifiers, or renames a quadrant label, fails these.
    // -----------------------------------------------------------------------

    fn fixture_fetched_at() -> DateTime<Utc> {
        t(2026, 5, 20, 12, 0)
    }

    /// Construct a Snapshot with every quadrant populated. Numbers chosen so
    /// the estimate ($4.20) and the real overage ($20.92) are distinct - i.e.
    /// the kind of layout where a careless reader could confuse the two.
    fn fully_populated_snapshot() -> Snapshot {
        let now = fixture_fetched_at();
        let mut snap = Snapshot::empty(now);

        snap.claude_oauth = Some(ClaudeOAuthSnapshot {
            cadences: vec![
                CadenceBar {
                    key: "five_hour".to_string(),
                    display_label: "Current 5-hour session".to_string(),
                    utilization_percent: 42.5,
                    resets_at: now + Duration::hours(2),
                },
                CadenceBar {
                    key: "seven_day".to_string(),
                    display_label: "Weekly".to_string(),
                    utilization_percent: 18.3,
                    resets_at: now + Duration::days(3),
                },
            ],
            extra_usage: Some(ExtraUsage {
                is_enabled: true,
                monthly_limit_micro_usd: 25_000_000, // $25.00
                used_credits_micro_usd: 20_920_000,  // $20.92
                utilization_percent: 83.7,
                currency: "USD".to_string(),
            }),
            subscription_type: Some("max_5x".to_string()),
            rate_limit_tier: Some("default".to_string()),
            org_uuid: None,
            fetched_at: now,
        });

        snap.claude_jsonl = Some(JsonlSnapshot {
            files_scanned: 3,
            window: WindowSummary {
                window_start: now - Duration::hours(5),
                total_events_in_window: 12,
                total_tokens_in_window: 45_000,
                recent_burn_tokens_per_min: Some(1234.0),
                by_model: vec![ByModel {
                    model: "claude-sonnet-4-6".to_string(),
                    events: 12,
                    total_tokens: 45_000,
                }],
            },
        });

        // Estimate dollar ($4.20) is intentionally distinct from the real
        // overage ($20.92) so any conflation in the label discipline is visible.
        snap.anthropic_api_cost = Some(Cost {
            per_model: vec![ModelCost {
                model: "claude-sonnet-4-6".to_string(),
                event_count: 12,
                input_micro_usd: 1_000_000,
                output_micro_usd: 3_000_000,
                cache_creation_micro_usd: 0,
                cache_read_micro_usd: 200_000,
                total_micro_usd: 4_200_000, // $4.20
            }],
            total_micro_usd: 4_200_000,
            skipped_models: vec![],
            total_event_count: 12,
            unparsed_event_count: 0,
        });

        snap.codex_quota = Some(CodexQuotaSnapshot {
            observed_at: now - Duration::minutes(5),
            session_id: "00000000-0000-7000-8000-000000000001".to_string(),
            primary: RateLimitWindow {
                used_percent: 17.5,
                window_duration_minutes: 10_080,
                resets_at: now + Duration::days(5),
            },
            secondary: None,
            plan_type: "go".to_string(),
            rate_limit_reached: false,
        });

        snap.openai = Some(OpenAiCosts {
            start_time: t(2026, 5, 1, 0, 0),
            end_time: now,
            total_micro_usd: 123_450_000,
            by_line_item: vec![LineItemCost {
                line_item: "gpt-5".to_string(),
                amount_micro_usd: 123_450_000,
            }],
            truncated: false,
            fetched_at: now,
        });

        snap
    }

    fn render_compact(snap: &Snapshot) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_compact(snap, &mut buf).expect("write_compact ok");
        String::from_utf8(buf).expect("compact output is UTF-8")
    }

    fn render_sections(snap: &Snapshot, verbose: bool) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_sections(snap, verbose, &mut buf).expect("write_sections ok");
        String::from_utf8(buf).expect("sections output is UTF-8")
    }

    #[test]
    fn compact_view_keeps_four_tiers_visibly_distinct() {
        let mut snap = fully_populated_snapshot();
        // Add pace data so the Pace: line appears.
        snap.pace = vec![state_coordinator::WindowPace {
            key: "five_hour".into(),
            used_fraction: 0.82,
            elapsed_fraction: 0.40,
            ratio: Some(2.05),
        }];
        let out = render_compact(&snap);

        // R1: column header must say "API $ (real billed)", not just "API $".
        assert!(
            out.contains("API $ (real billed)"),
            "compact header must say 'API $ (real billed)':\n{out}"
        );

        // R1: the matrix cell for Anthropic API $ must show "overage (real)"
        // when extra_usage is enabled, NOT the JSONL estimate.
        assert!(
            out.contains("overage (real)"),
            "compact Anthropic-cost cell must carry 'overage (real)' when extra_usage enabled:\n{out}"
        );

        // R1: the JSONL estimate must NOT appear in the matrix - it lives on
        // the separate Subscription leverage line.
        assert!(
            !out.contains("est-leverage"),
            "the matrix must not contain 'est-leverage' - it belongs on the leverage line:\n{out}"
        );

        // Subscription leverage line - shows the JSONL estimate outside the matrix.
        assert!(
            out.contains("Subscription leverage:"),
            "compact must have a 'Subscription leverage:' line:\n{out}"
        );
        assert!(
            out.contains("NOT billed"),
            "Subscription leverage line must carry 'NOT billed' qualifier:\n{out}"
        );

        // Pace line - must show used% / elapsed% and ratio.
        assert!(
            out.contains("Pace:"),
            "compact must have a 'Pace:' line when pace is non-empty:\n{out}"
        );
        assert!(
            out.contains("5h"),
            "Pace line must show the short cadence key '5h':\n{out}"
        );

        // Tier 3a - Anthropic server quota. The "(oauth)" suffix is the
        // wire-source tag.
        assert!(
            out.contains("(oauth)"),
            "compact Anthropic-quota cell must carry the (oauth) source tag\n{out}"
        );

        // Tier 3b - Codex server quota. "(codex ..." carries the source.
        assert!(
            out.contains("(codex go"),
            "compact OpenAI-quota cell must carry the (codex ...) source tag\n{out}"
        );

        // Tier 4 - OpenAI real billed spend, from the Admin Costs endpoint.
        assert!(
            out.contains("(admin costs)"),
            "compact OpenAI-cost cell must carry the (admin costs) source tag\n{out}"
        );

        // Legend re-establishes the confidence split.
        assert!(
            out.contains("real billed spend"),
            "legend must label OpenAI as real billed spend:\n{out}"
        );
        assert!(
            out.contains("Subscription leverage"),
            "legend must mention 'Subscription leverage':\n{out}"
        );
        assert!(
            out.contains("never charged"),
            "legend must say estimate is 'never charged':\n{out}"
        );

        // Codex age tag - observed_at is 5min behind fetched_at, so the
        // ", 5m old" suffix must appear. Pins the new freshness signal.
        assert!(
            out.contains(", 5m old"),
            "compact codex cell must surface the snapshot age:\n{out}"
        );
    }

    #[test]
    fn compact_codex_quota_short_window_not_zero_days() {
        let now = fixture_fetched_at();
        let mut snap = fully_populated_snapshot();
        if let Some(q) = snap.codex_quota.as_mut() {
            q.primary.window_duration_minutes = 300; // 5h
            q.primary.resets_at = now + Duration::hours(3); // still live
        }
        let cell = compact_codex_quota(&snap);
        assert!(
            cell.contains("5h"),
            "5-hour window must render '5h':\n{cell}"
        );
        assert!(
            !cell.contains("0d"),
            "must not collapse a 5h window to '0d':\n{cell}"
        );
        assert!(
            cell.starts_with('✓'),
            "a live window keeps the ✓ marker:\n{cell}"
        );
    }

    #[test]
    fn compact_codex_quota_expired_window_marked_stale() {
        let now = fixture_fetched_at();
        let mut snap = fully_populated_snapshot();
        if let Some(q) = snap.codex_quota.as_mut() {
            // resets_at before fetched_at: the rollout outlived its window.
            q.primary.resets_at = now - Duration::hours(1);
        }
        let cell = compact_codex_quota(&snap);
        assert!(
            !cell.starts_with('✓'),
            "an expired window must not show a confident ✓:\n{cell}"
        );
        assert!(
            cell.contains("stale"),
            "an expired window must be labeled stale:\n{cell}"
        );
    }

    #[test]
    fn compact_anthropic_cost_absent_extra_usage_shows_not_available() {
        // When extra_usage is absent or disabled, the Anthropic API $ cell
        // must show "- not available", never the JSONL estimate.
        let mut snap = fully_populated_snapshot();
        if let Some(oauth) = snap.claude_oauth.as_mut() {
            oauth.extra_usage = None;
        }
        let out = render_compact(&snap);
        assert!(
            out.contains("- not available"),
            "Anthropic cost cell must show '- not available' when extra_usage is absent:\n{out}"
        );
        // The JSONL estimate must still appear on the leverage line, not in the matrix.
        assert!(
            out.contains("Subscription leverage:"),
            "Subscription leverage line must still appear when extra_usage is absent:\n{out}"
        );
        assert!(
            !out.contains("est-leverage"),
            "matrix must never contain est-leverage:\n{out}"
        );
    }

    #[test]
    fn compact_pace_line_absent_when_pace_is_empty() {
        let mut snap = fully_populated_snapshot();
        snap.pace = vec![];
        let out = render_compact(&snap);
        assert!(
            !out.contains("Pace:"),
            "Pace: line must be absent when pace vec is empty:\n{out}"
        );
    }

    #[test]
    fn compact_pace_line_ratio_none_shows_dash() {
        let mut snap = fully_populated_snapshot();
        snap.pace = vec![state_coordinator::WindowPace {
            key: "five_hour".into(),
            used_fraction: 0.10,
            elapsed_fraction: 0.00,
            ratio: None,
        }];
        let out = render_compact(&snap);
        assert!(out.contains("Pace:"), "Pace: line must appear:\n{out}");
        assert!(
            out.contains("(-)"),
            "ratio: None must render as (-):\n{out}"
        );
    }

    #[test]
    fn compact_leverage_line_absent_when_no_jsonl_events() {
        let mut snap = fully_populated_snapshot();
        // Zero events → leverage line must be absent.
        if let Some(cost) = snap.anthropic_api_cost.as_mut() {
            cost.total_event_count = 0;
        }
        let out = render_compact(&snap);
        assert!(
            !out.contains("Subscription leverage:"),
            "Subscription leverage line must be absent when total_event_count == 0:\n{out}"
        );
    }

    #[test]
    fn compact_leverage_line_surfaces_cost_synthesis_error() {
        // The measured-only matrix cell can't show a cost-synthesis failure;
        // the leverage line must surface it instead of going silent.
        let mut snap = fully_populated_snapshot();
        snap.anthropic_api_cost = None;
        snap.anthropic_api_cost_error = Some("price table missing".to_string());
        let out = render_compact(&snap);
        assert!(
            out.contains("Subscription leverage: ✗ cost synthesis failed"),
            "cost synthesis error must be surfaced on the leverage line:\n{out}"
        );
    }

    #[test]
    fn compact_leverage_line_surfaces_jsonl_load_error() {
        let mut snap = fully_populated_snapshot();
        snap.anthropic_api_cost = None;
        snap.anthropic_api_cost_error = None;
        snap.claude_jsonl_error = Some("permission denied".to_string());
        let out = render_compact(&snap);
        assert!(
            out.contains("Subscription leverage: ✗ jsonl load failed"),
            "jsonl load error must be surfaced on the leverage line:\n{out}"
        );
    }

    #[test]
    fn compact_view_does_not_conflate_estimate_and_real_spend() {
        let snap = fully_populated_snapshot();
        let out = render_compact(&snap);

        // The JSONL estimate ($4.20) must never appear in the matrix cells.
        // It must only appear on the Subscription leverage line (outside matrix).
        for line in out.lines() {
            if line.contains("$4.20") {
                assert!(
                    line.contains("Subscription leverage") || line.contains("leverage"),
                    "every line carrying the estimate $ must be on the leverage line; offending line: {line:?}\nfull output:\n{out}"
                );
            }
        }
    }

    #[test]
    fn sections_view_keeps_estimate_and_overage_blocks_apart() {
        let snap = fully_populated_snapshot();
        let out = render_sections(&snap, false);

        // The EXTRA USAGE block header MUST carry the "REAL billed spend"
        // qualifier. This is the single most important label in the file.
        assert!(
            out.contains("EXTRA USAGE (pay-as-you-go overage - REAL billed spend"),
            "EXTRA USAGE section must carry the REAL billed spend qualifier:\n{out}"
        );

        // The estimate block MUST carry "ESTIMATE ONLY" and the leverage
        // disclaimer on the value line.
        assert!(
            out.contains("ANTHROPIC API COST - ESTIMATE ONLY"),
            "ANTHROPIC API COST section header must carry the ESTIMATE ONLY tag:\n{out}"
        );
        assert!(
            out.contains("subscription leverage, NOT money billed"),
            "estimate value line must carry the leverage-not-billed qualifier:\n{out}"
        );

        // Cross-link from estimate block back to extra-usage block - keeps
        // a reader who lands on the estimate first oriented to where the
        // real money is.
        assert!(
            out.contains("Real out-of-pocket spend is in the EXTRA USAGE block"),
            "estimate block must point at the EXTRA USAGE block for real spend:\n{out}"
        );

        // OpenAI spend block is a real bill (admin costs). Must not be
        // labelled as anything that could be misread as an estimate.
        assert!(
            out.contains("OPENAI SPEND"),
            "OPENAI SPEND section must be present:\n{out}"
        );
        assert!(
            !out.contains("OPENAI SPEND: estimate"),
            "OpenAI spend must never be tagged as an estimate:\n{out}"
        );

        // Codex header - plan + age. The plan_type "go" + observed_at 5min
        // behind fetched_at means the age suffix must render.
        assert!(
            out.contains("OPENAI CODEX QUOTA (plan: go,"),
            "codex header must declare plan:\n{out}"
        );
        assert!(
            out.contains(", 5m old)"),
            "codex sections header must carry the freshness tag:\n{out}"
        );
    }

    #[test]
    fn sections_handles_disabled_extra_usage_without_dropping_estimate_qualifier() {
        // When extra_usage is None / disabled, the EXTRA USAGE block goes
        // away - but the estimate block MUST still carry its qualifier
        // (otherwise a Pro user sees a bare "$4.20" and might assume real
        // spend). This is the regression the labeling discipline exists to
        // prevent.
        let mut snap = fully_populated_snapshot();
        if let Some(oauth) = snap.claude_oauth.as_mut() {
            oauth.extra_usage = None;
        }

        let out = render_sections(&snap, false);
        assert!(
            !out.contains("EXTRA USAGE (pay-as-you-go"),
            "EXTRA USAGE block must be hidden when extra_usage is None:\n{out}"
        );
        assert!(
            out.contains("ANTHROPIC API COST - ESTIMATE ONLY"),
            "estimate block qualifier must survive extra_usage going away:\n{out}"
        );
        assert!(
            out.contains("subscription leverage, NOT money billed"),
            "leverage qualifier must survive extra_usage going away:\n{out}"
        );
    }
}
