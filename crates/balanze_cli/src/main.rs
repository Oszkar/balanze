//! Balanze CLI — composes the backend crates into a single status view.
//!
//! Subcommands:
//!   balanze                       Print pretty status (default)
//!   balanze status [--json]       Same as above; --json is machine-readable
//!   balanze set-openai-key        Read sk-... from stdin, store in OS keychain
//!   balanze clear-openai-key      Remove the OpenAI key from the keychain
//!   balanze settings              Print current settings.json contents
//!   balanze help                  This help
//!
//! When the Tauri front-end lands, the same composition logic will live
//! behind the `get_snapshot` IPC command in `src-tauri`. This CLI is the
//! reference implementation and a useful dev tool in its own right.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

use anthropic_oauth::{fetch_usage, load as load_credentials, ClaudeOAuthSnapshot, DEFAULT_API_BASE as ANTHROPIC_API_BASE};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use claude_parser::{find_jsonl_files, parse_str, UsageEvent};
use openai_client::{costs_this_month, OpenAiCosts, OpenAiError, DEFAULT_API_BASE as OPENAI_API_BASE};
use serde::Serialize;
use tracing::{info, warn};

#[derive(Serialize)]
struct CliSnapshot {
    fetched_at: DateTime<Utc>,
    claude_oauth: Option<ClaudeOAuthSnapshot>,
    claude_oauth_error: Option<String>,
    claude_jsonl: Option<JsonlSummary>,
    claude_jsonl_error: Option<String>,
    openai: Option<OpenAiCosts>,
    /// `None` means: OpenAI not configured. `Some` with error string means
    /// configured but the fetch failed.
    openai_error: Option<String>,
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

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("status");

    let result = match cmd {
        "status" | "--json" => cmd_status(&args),
        "set-openai-key" => cmd_set_openai_key(),
        "clear-openai-key" => cmd_clear_openai_key(),
        "settings" => cmd_settings(),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}");
            print_help();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_status(args: &[String]) -> Result<()> {
    let json_mode = args.iter().any(|a| a == "--json");
    let snapshot = tokio::runtime::Runtime::new()?.block_on(build_snapshot());

    if json_mode {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else {
        print_pretty(&snapshot);
    }
    Ok(())
}

fn cmd_set_openai_key() -> Result<()> {
    // Accept the key as a positional argument (`balanze set-openai-key sk-…`)
    // or, if no argv is provided, read one line from stdin. We always use
    // `read_line` regardless of TTY status — `read_to_string` waited for EOF
    // and made the command look hung under `cargo run` on Windows.
    let args: Vec<String> = env::args().collect();
    let argv_key = args.iter().skip(2).find(|s| !s.is_empty()).cloned();

    let raw = if let Some(k) = argv_key {
        k
    } else {
        eprint!("Paste your OpenAI API key (sk-...) and press Enter: ");
        let _ = io::stderr().flush();
        let mut input = String::new();
        let n = io::stdin().read_line(&mut input)?;
        if n == 0 {
            return Err(anyhow!(
                "stdin closed without input. Tip: pass the key as an argument instead — `balanze set-openai-key sk-...`"
            ));
        }
        input
    };

    let key = raw.trim().to_string();
    if key.is_empty() {
        return Err(anyhow!("no key provided"));
    }
    if !key.starts_with("sk-") {
        return Err(anyhow!(
            "key doesn't look like an OpenAI key (expected to start with `sk-`)"
        ));
    }
    // Warn about non-admin keys but don't block — the API will reject them
    // and the user will see the specific error in the next status fetch.
    let is_admin_key = key.starts_with("sk-admin-");
    if !is_admin_key {
        eprintln!(
            "Heads up: this doesn't look like an admin key. The organization/costs"
        );
        eprintln!(
            "endpoint Balanze uses requires an admin key (`sk-admin-…`); project keys"
        );
        eprintln!(
            "(`sk-proj-…`) and service-account keys will return 403 here. Create an"
        );
        eprintln!(
            "admin key at https://platform.openai.com/settings/organization/admin-keys"
        );
        eprintln!("and replace this one if the next `balanze` run shows an error.");
    }

    keychain::set(keychain::keys::OPENAI_API_KEY, &key)?;

    let mut s = settings::load().unwrap_or_default();
    s.providers.openai_enabled = true;
    settings::save(&s)?;

    eprintln!("Stored OpenAI key in the OS keychain ({} bytes).", key.len());
    if is_admin_key {
        eprintln!("Run `balanze` to verify the tile shows spend data.");
    }
    Ok(())
}

fn cmd_clear_openai_key() -> Result<()> {
    keychain::delete(keychain::keys::OPENAI_API_KEY)?;
    let mut s = settings::load().unwrap_or_default();
    s.providers.openai_enabled = false;
    settings::save(&s)?;
    eprintln!("Removed OpenAI key from the keychain.");
    Ok(())
}

fn cmd_settings() -> Result<()> {
    let s = settings::load()?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    let path = settings::default_path()?;
    eprintln!("(loaded from: {})", path.display());
    Ok(())
}

fn print_help() {
    eprintln!("Balanze — local-first AI usage tracker.");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  balanze                       Print pretty status (default)");
    eprintln!("  balanze status [--json]       Same as above; --json is machine-readable");
    eprintln!("  balanze set-openai-key [KEY]  Store KEY in the OS keychain. Reads from stdin if KEY is omitted.");
    eprintln!("  balanze clear-openai-key      Remove the OpenAI key from the keychain");
    eprintln!("  balanze settings              Print current settings.json contents");
    eprintln!("  balanze help                  This help");
    eprintln!();
    eprintln!("Tip: run via `cargo run --release -p balanze_cli -- <subcommand>` (note the `--`).");
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
    let (openai, openai_error) = match fetch_openai().await {
        Ok(Some(g)) => (Some(g), None),
        Ok(None) => (None, None),
        Err(e) => {
            warn!("OpenAI source failed: {e}");
            (None, Some(e.to_string()))
        }
    };
    CliSnapshot {
        fetched_at: now,
        claude_oauth,
        claude_oauth_error,
        claude_jsonl,
        claude_jsonl_error,
        openai,
        openai_error,
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
        ANTHROPIC_API_BASE,
        &oauth.access_token,
        oauth.subscription_type,
        oauth.rate_limit_tier,
    )
    .await?;
    info!("oauth: fetched {} cadence bars", snapshot.cadences.len());
    Ok(snapshot)
}

/// Fetch this-month OpenAI costs if the user has configured an admin key.
/// Returns `Ok(None)` when nothing is configured (so the CLI can print a
/// "not configured" hint rather than a scary error). Returns `Err` only for
/// real failures (401, 403, network, etc.).
async fn fetch_openai() -> Result<Option<OpenAiCosts>> {
    let key = match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(k) => k,
        Err(keychain::KeychainError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let client = reqwest::Client::builder()
        .user_agent("balanze-cli/0.1.0")
        .build()?;
    match costs_this_month(&client, OPENAI_API_BASE, &key).await {
        Ok(costs) => {
            info!(
                "openai: fetched costs total_usd={} buckets={} truncated={}",
                costs.total_usd,
                costs.by_line_item.len(),
                costs.truncated
            );
            Ok(Some(costs))
        }
        Err(OpenAiError::AuthInvalid { .. }) => Err(anyhow!(
            "OpenAI admin key rejected (HTTP 401). Run `balanze set-openai-key` with a fresh `sk-admin-…` key."
        )),
        Err(OpenAiError::InsufficientScope { .. }) => Err(anyhow!(
            "OpenAI returned 403. organization/costs requires an admin API key (`sk-admin-…`), not a project or service-account key. Generate one at https://platform.openai.com/settings/organization/admin-keys."
        )),
        Err(e) => Err(e.into()),
    }
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
    Err(anyhow!(
        "claude projects directory not found; searched {:?}",
        candidates
    ))
}

fn print_pretty(snapshot: &CliSnapshot) {
    let _ = io::stdout().flush();
    println!("=== Balanze Status ===");
    println!("fetched: {}", snapshot.fetched_at.format("%Y-%m-%d %H:%M:%S UTC"));
    println!();

    // Claude OAuth (cadence bars)
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
        // extra_usage block intentionally suppressed; see commit e14365f.
        let _ = &oauth.extra_usage;
    } else if let Some(err) = &snapshot.claude_oauth_error {
        println!("CADENCE BARS: unavailable — {err}");
    }

    // OpenAI monthly costs (from Admin API)
    println!();
    if let Some(costs) = &snapshot.openai {
        println!(
            "OPENAI SPEND ({} – {}):",
            costs.start_time.format("%Y-%m-%d"),
            costs.end_time.format("%Y-%m-%d"),
        );
        let suffix = if costs.truncated { "  (partial; more pages available)" } else { "" };
        println!("  Total: ${:.2}{suffix}", costs.total_usd);
        if !costs.by_line_item.is_empty() {
            println!();
            println!("  By line item:");
            for item in costs.by_line_item.iter().take(10) {
                println!("    {:36}  ${:>10.4}", item.line_item, item.amount_usd);
            }
            if costs.by_line_item.len() > 10 {
                println!("    … ({} more)", costs.by_line_item.len() - 10);
            }
        }
    } else if let Some(err) = &snapshot.openai_error {
        println!("OPENAI SPEND: unavailable — {err}");
    } else {
        println!("OPENAI SPEND: not configured");
        println!("  Run `balanze set-openai-key` to add a `sk-admin-…` key.");
        println!("  Create one at https://platform.openai.com/settings/organization/admin-keys");
    }

    // Claude Code JSONL activity
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
            Some(rate) => println!(
                "  recent burn:       ~{} tokens/min (last 30 min)",
                fmt_int(rate as u64)
            ),
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
