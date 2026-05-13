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
use std::io::{self, Read, Write};
use std::process::ExitCode;

use anthropic_oauth::{fetch_usage, load as load_credentials, ClaudeOAuthSnapshot, DEFAULT_API_BASE as ANTHROPIC_API_BASE};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use claude_parser::{find_jsonl_files, parse_str, UsageEvent};
use openai_client::{fetch_credit_grants, CreditGrants, OpenAiError, DEFAULT_API_BASE as OPENAI_API_BASE};
use serde::Serialize;
use tracing::{info, warn};

#[derive(Serialize)]
struct CliSnapshot {
    fetched_at: DateTime<Utc>,
    claude_oauth: Option<ClaudeOAuthSnapshot>,
    claude_oauth_error: Option<String>,
    claude_jsonl: Option<JsonlSummary>,
    claude_jsonl_error: Option<String>,
    openai: Option<CreditGrants>,
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
    eprintln!("Paste your OpenAI API key (sk-...) and press Enter:");
    let mut input = String::new();
    // If stdin is a pipe, read everything; otherwise read one line.
    if atty_isatty() {
        io::stdin().read_line(&mut input)?;
    } else {
        io::stdin().read_to_string(&mut input)?;
    }
    let key = input.trim().to_string();
    if key.is_empty() {
        return Err(anyhow!("no key provided"));
    }
    if !key.starts_with("sk-") {
        return Err(anyhow!(
            "key doesn't look like an OpenAI key (expected to start with `sk-`)"
        ));
    }
    // Warn about project keys but don't block — let the user decide.
    let is_project_key = key.starts_with("sk-proj-");
    if is_project_key {
        eprintln!(
            "Heads up: keys starting with `sk-proj-` (project keys) do not have access to the billing"
        );
        eprintln!(
            "credit_grants endpoint. Balanze will store this key but the OpenAI tile will show a"
        );
        eprintln!(
            "403 error until you replace it with a legacy/user key from your OpenAI account settings."
        );
    }

    keychain::set(keychain::keys::OPENAI_API_KEY, &key)?;

    let mut s = settings::load().unwrap_or_default();
    s.providers.openai_enabled = true;
    settings::save(&s)?;

    eprintln!("Stored OpenAI key in the OS keychain ({} bytes).", key.len());
    if !is_project_key {
        eprintln!("Run `balanze` to verify the tile shows credit data.");
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
    eprintln!("  balanze                  Print pretty status (default)");
    eprintln!("  balanze status [--json]  Same as above; --json is machine-readable");
    eprintln!("  balanze set-openai-key   Read sk-... from stdin, store in OS keychain");
    eprintln!("  balanze clear-openai-key Remove the OpenAI key from the keychain");
    eprintln!("  balanze settings         Print current settings.json contents");
    eprintln!("  balanze help             This help");
}

/// Cheap stdin-is-a-tty check. Avoids pulling in the `atty` crate; assumes
/// Windows is a TTY for `read_line` purposes, which is correct for our use case.
fn atty_isatty() -> bool {
    // We always want the read_line behavior unless stdin was redirected.
    // The simplest portable check is "are we running in an interactive shell" —
    // approximated by checking if stdin is a tty via std::io::IsTerminal.
    use std::io::IsTerminal;
    io::stdin().is_terminal()
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

/// Fetch OpenAI credit grants if the user has configured an API key.
/// Returns `Ok(None)` when nothing is configured (so the CLI can print a
/// "not configured" hint rather than a scary error). Returns `Err` only for
/// real failures (401, 403, network, etc.).
async fn fetch_openai() -> Result<Option<CreditGrants>> {
    let key = match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(k) => k,
        Err(keychain::KeychainError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let client = reqwest::Client::builder()
        .user_agent("balanze-cli/0.1.0")
        .build()?;
    match fetch_credit_grants(&client, OPENAI_API_BASE, &key).await {
        Ok(grants) => {
            info!(
                "openai: fetched grants total_granted={} total_used={}",
                grants.total_granted_usd, grants.total_used_usd
            );
            Ok(Some(grants))
        }
        Err(OpenAiError::AuthExpired { .. }) => Err(anyhow!(
            "OpenAI API key rejected (HTTP 401). Run `balanze set-openai-key` to update."
        )),
        Err(OpenAiError::ForbiddenProjectKey { .. }) => Err(anyhow!(
            "OpenAI returned 403. The credit_grants endpoint requires a legacy/user API key; project keys (`sk-proj-…`) don't have billing access. Generate a legacy key at platform.openai.com/account/api-keys."
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

    // OpenAI credit grants
    println!();
    if let Some(grants) = &snapshot.openai {
        let used_pct = if grants.total_granted_usd > 0.0 {
            (grants.total_used_usd / grants.total_granted_usd) * 100.0
        } else {
            0.0
        };
        println!("OPENAI API CREDITS:");
        println!(
            "  Used ${:.2} of ${:.2} ({:.1}%)  —  ${:.2} available",
            grants.total_used_usd, grants.total_granted_usd, used_pct, grants.total_available_usd
        );
        if let Some(expires) = grants.next_grant_expiry {
            let in_dur = expires.signed_duration_since(snapshot.fetched_at);
            println!(
                "  Next grant expires: {} (in {})",
                expires.format("%Y-%m-%d"),
                pretty_duration(in_dur)
            );
        }
    } else if let Some(err) = &snapshot.openai_error {
        println!("OPENAI API CREDITS: unavailable — {err}");
    } else {
        println!("OPENAI API CREDITS: not configured");
        println!("  Run `balanze set-openai-key` to add a legacy/user sk-... key.");
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
