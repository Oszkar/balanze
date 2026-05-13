//! Balanze CLI — composes the backend crates into a single status view.
//!
//! Run with no args to print a pretty snapshot, or `--json` for
//! machine-readable output. Each data source degrades independently:
//! if OAuth fails, JSONL summary still prints, and vice versa.
//!
//! When the Tauri front-end lands, the same composition logic will live
//! behind the `get_snapshot` IPC command in `src-tauri`. This CLI is the
//! reference implementation and a useful dev tool in its own right.

use std::collections::BTreeMap;
use std::env;
use std::fs;

use anthropic_oauth::{fetch_usage, load as load_credentials, ClaudeOAuthSnapshot, DEFAULT_API_BASE};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use claude_parser::{find_jsonl_files, parse_str, UsageEvent};
use serde::Serialize;
use tracing::{info, warn};

#[derive(Serialize)]
struct CliSnapshot {
    fetched_at: DateTime<Utc>,
    claude_oauth: Option<ClaudeOAuthSnapshot>,
    claude_oauth_error: Option<String>,
    claude_jsonl: Option<JsonlSummary>,
    claude_jsonl_error: Option<String>,
}

#[derive(Serialize)]
struct JsonlSummary {
    files_scanned: usize,
    window_start: DateTime<Utc>,
    /// Events from the last 5 hours (matching OAuth's five_hour cadence).
    total_events_in_window: usize,
    /// All token categories (input + output + cache_creation + cache_read), summed.
    total_tokens_in_window: u64,
    /// Tokens-per-minute averaged over the last 30 minutes of events.
    /// `None` if too few events to compute meaningfully.
    recent_burn_tokens_per_min: Option<f64>,
    /// Per-model breakdown over the 5h window. Sorted by total tokens descending.
    by_model: Vec<ByModel>,
}

#[derive(Serialize)]
struct ByModel {
    model: String,
    events: usize,
    total_tokens: u64,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let json_mode = env::args().any(|a| a == "--json");

    let snapshot = tokio::runtime::Runtime::new()?.block_on(build_snapshot());

    if json_mode {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else {
        print_pretty(&snapshot);
    }
    Ok(())
}

async fn build_snapshot() -> CliSnapshot {
    let now = Utc::now();
    let (claude_oauth, claude_oauth_error) = match fetch_oauth().await {
        Ok(s) => (Some(s), None),
        Err(e) => {
            warn!("OAuth source failed: {e}");
            (None, Some(e.to_string()))
        }
    };
    let (claude_jsonl, claude_jsonl_error) = match build_jsonl_summary(now) {
        Ok(s) => (Some(s), None),
        Err(e) => {
            warn!("JSONL source failed: {e}");
            (None, Some(e.to_string()))
        }
    };
    CliSnapshot {
        fetched_at: now,
        claude_oauth,
        claude_oauth_error,
        claude_jsonl,
        claude_jsonl_error,
    }
}

async fn fetch_oauth() -> Result<ClaudeOAuthSnapshot> {
    let creds = load_credentials()?;
    let oauth = creds.claude_ai_oauth;
    let client = reqwest::Client::builder()
        .user_agent("balanze-cli/0.1.0")
        .build()?;
    let snapshot = fetch_usage(
        &client,
        DEFAULT_API_BASE,
        &oauth.access_token,
        oauth.subscription_type,
        oauth.rate_limit_tier,
    )
    .await?;
    info!("oauth: fetched {} cadence bars", snapshot.cadences.len());
    Ok(snapshot)
}

fn build_jsonl_summary(now: DateTime<Utc>) -> Result<JsonlSummary> {
    let claude_dir = locate_claude_projects_dir()?;
    let files = find_jsonl_files(&claude_dir)?;
    info!(
        "jsonl: scanning {} files under {}",
        files.len(),
        claude_dir.display()
    );

    let window_start = now - Duration::hours(5);
    let burn_window_start = now - Duration::minutes(30);
    let mut all_events: Vec<UsageEvent> = Vec::new();

    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!("jsonl: skipping {} ({e})", path.display());
                continue;
            }
        };
        match parse_str(&content) {
            Ok(events) => all_events.extend(events),
            Err(e) => warn!("jsonl: parse error in {} ({e})", path.display()),
        }
    }

    let mut by_model_map: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut total_events_in_window = 0usize;
    let mut total_tokens_in_window: u64 = 0;
    let mut burn_tokens: u64 = 0;
    let mut burn_events: usize = 0;

    for ev in &all_events {
        if ev.ts >= window_start {
            total_events_in_window += 1;
            let tokens = ev.total_tokens();
            total_tokens_in_window = total_tokens_in_window.saturating_add(tokens);
            let entry = by_model_map.entry(ev.model.clone()).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry.1.saturating_add(tokens);
        }
        if ev.ts >= burn_window_start {
            burn_events += 1;
            burn_tokens = burn_tokens.saturating_add(ev.total_tokens());
        }
    }

    // Burn rate: tokens per minute over last 30 minutes. Require at least 3 events.
    let recent_burn_tokens_per_min = if burn_events >= 3 {
        Some(burn_tokens as f64 / 30.0)
    } else {
        None
    };

    let mut by_model: Vec<ByModel> = by_model_map
        .into_iter()
        .map(|(model, (events, total_tokens))| ByModel {
            model,
            events,
            total_tokens,
        })
        .collect();
    by_model.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));

    Ok(JsonlSummary {
        files_scanned: files.len(),
        window_start,
        total_events_in_window,
        total_tokens_in_window,
        recent_burn_tokens_per_min,
        by_model,
    })
}

fn locate_claude_projects_dir() -> Result<std::path::PathBuf> {
    // Check XDG first, then ~/.claude/projects, then ~/.config/claude/projects.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        candidates.push(std::path::PathBuf::from(xdg).join("claude").join("projects"));
    }
    if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        let home = std::path::PathBuf::from(home);
        candidates.push(home.join(".claude").join("projects"));
        candidates.push(home.join(".config").join("claude").join("projects"));
    }
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    Err(anyhow::anyhow!(
        "claude projects directory not found; searched {:?}",
        candidates
    ))
}

fn print_pretty(snapshot: &CliSnapshot) {
    println!("=== Balanze Status ===");
    println!("fetched: {}", snapshot.fetched_at.format("%Y-%m-%d %H:%M:%S UTC"));
    println!();

    if let Some(oauth) = &snapshot.claude_oauth {
        println!(
            "subscription: {} ({})",
            oauth.subscription_type.as_deref().unwrap_or("?"),
            oauth.rate_limit_tier.as_deref().unwrap_or("?"),
        );
        if let Some(uuid) = &oauth.org_uuid {
            println!("org uuid:     {uuid}");
        }
        println!();
        println!("CADENCE BARS (from Anthropic OAuth):");
        if oauth.cadences.is_empty() {
            println!("  (none reported)");
        }
        for cad in &oauth.cadences {
            let resets_in = cad.resets_at.signed_duration_since(snapshot.fetched_at);
            println!(
                "  {:32}  {:>6.2}%   resets in {}",
                cad.display_label,
                cad.utilization_percent,
                pretty_duration(resets_in)
            );
        }
        if let Some(extra) = &oauth.extra_usage {
            let used = (extra.used_credits_micro_usd as f64) / 1_000_000.0;
            let limit = (extra.monthly_limit_micro_usd as f64) / 1_000_000.0;
            let remaining = limit - used;
            println!();
            println!(
                "EXTRA USAGE: {}",
                if extra.is_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            println!(
                "  Used {:.2} {} of {:.2} {} ({:.1}%)  — {:.2} {} remaining",
                used, extra.currency, limit, extra.currency, extra.utilization_percent, remaining, extra.currency
            );
        }
    } else if let Some(err) = &snapshot.claude_oauth_error {
        println!("CADENCE BARS: unavailable — {err}");
    }

    println!();
    if let Some(jsonl) = &snapshot.claude_jsonl {
        println!("CLAUDE CODE ACTIVITY (last 5h, from local JSONL):");
        println!("  files scanned:     {}", jsonl.files_scanned);
        println!("  events in window:  {}", jsonl.total_events_in_window);
        println!(
            "  tokens in window:  {}",
            fmt_int(jsonl.total_tokens_in_window)
        );
        match jsonl.recent_burn_tokens_per_min {
            Some(rate) => println!("  recent burn:       ~{} tokens/min (last 30 min)", fmt_int(rate as u64)),
            None => println!("  recent burn:       (too few events in last 30 min)"),
        }
        if !jsonl.by_model.is_empty() {
            println!();
            println!("  By model:");
            for m in &jsonl.by_model {
                println!(
                    "    {:36}  events: {:>4}  tokens: {:>14}",
                    m.model,
                    m.events,
                    fmt_int(m.total_tokens)
                );
            }
        }
    } else if let Some(err) = &snapshot.claude_jsonl_error {
        println!("CLAUDE CODE ACTIVITY: unavailable — {err}");
    }
}

fn pretty_duration(d: Duration) -> String {
    if d.num_seconds() < 0 {
        return "(passed)".to_string();
    }
    let total_secs = d.num_seconds();
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn fmt_int(n: u64) -> String {
    // Comma-separated thousands. No locale dependency.
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}
