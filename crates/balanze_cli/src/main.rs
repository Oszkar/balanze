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

use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

use anthropic_oauth::{
    fetch_usage, load as load_credentials, ClaudeOAuthSnapshot,
    DEFAULT_API_BASE as ANTHROPIC_API_BASE,
};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use claude_parser::{dedup_events, find_claude_projects_dir, find_jsonl_files, parse_str, UsageEvent};
use openai_client::{
    costs_this_month, OpenAiCosts, OpenAiError, DEFAULT_API_BASE as OPENAI_API_BASE,
};
use state_coordinator::{JsonlSnapshot, Snapshot};
use tracing::{info, warn};
use window::{summarize_window, DEFAULT_BURN_WINDOW, DEFAULT_MIN_BURN_EVENTS, DEFAULT_WINDOW};

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
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let snapshot = tokio::runtime::Runtime::new()?.block_on(build_snapshot());

    if json_mode {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else {
        print_pretty(&snapshot, verbose);
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
    eprintln!("  balanze status [--json] [-v]  Same as above; --json is machine-readable;");
    eprintln!("                                -v / --verbose adds account-identifying fields");
    eprintln!("                                (org uuid) — safe to share at home, dox-y in public.");
    eprintln!("  balanze set-openai-key [KEY]  Store KEY in the OS keychain. Reads from stdin if KEY is omitted.");
    eprintln!("  balanze clear-openai-key      Remove the OpenAI key from the keychain");
    eprintln!("  balanze settings              Print current settings.json contents");
    eprintln!("  balanze help                  This help");
    eprintln!();
    eprintln!("Environment overrides:");
    eprintln!("  BALANZE_OPENAI_KEY            sk-admin-… admin key. Takes precedence over keychain.");
    eprintln!("                                Recommended on Windows until the keychain backend is");
    eprintln!("                                migrated to keyring v4 in v0.2.");
    eprintln!();
    eprintln!("Tip: run via `cargo run --release -p balanze_cli -- <subcommand>` (note the `--`).");
}

async fn build_snapshot() -> Snapshot {
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
    Snapshot {
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
///
/// Source order:
///   1. `BALANZE_OPENAI_KEY` env var (fallback while the keychain backend is
///      unreliable on Windows — see AGENTS.md known issues)
///   2. OS keychain entry `openai_api_key`
///   3. None → "not configured"
///
/// Returns `Ok(None)` when nothing is configured; `Err` only for real
/// fetch failures (401, 403, network, etc.).
async fn fetch_openai() -> Result<Option<OpenAiCosts>> {
    let key = if let Ok(env_key) = env::var("BALANZE_OPENAI_KEY") {
        if env_key.trim().is_empty() {
            return Ok(None);
        }
        env_key.trim().to_string()
    } else {
        match keychain::get(keychain::keys::OPENAI_API_KEY) {
            Ok(k) => k,
            Err(keychain::KeychainError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
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

fn build_jsonl_summary(now: DateTime<Utc>) -> Result<JsonlSnapshot> {
    let claude_dir = find_claude_projects_dir()?;
    let files = find_jsonl_files(&claude_dir)?;
    info!(
        "jsonl: scanning {} files under {}",
        files.len(),
        claude_dir.display()
    );

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

    let before = all_events.len();
    dedup_events(&mut all_events);
    let after = all_events.len();
    if before != after {
        info!(
            "jsonl: deduped {} → {} events ({} duplicates collapsed by (msg_id, req_id))",
            before,
            after,
            before - after
        );
    }

    let window = summarize_window(
        &all_events,
        now,
        DEFAULT_WINDOW,
        DEFAULT_BURN_WINDOW,
        DEFAULT_MIN_BURN_EVENTS,
    );

    Ok(JsonlSnapshot {
        files_scanned: files.len(),
        window,
    })
}

fn print_pretty(snapshot: &Snapshot, verbose: bool) {
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
        // org_uuid identifies the user's Anthropic consumer org. Useful for
        // bug reports but doxes the account when pasted publicly, so it's
        // gated behind --verbose / -v.
        if verbose {
            if let Some(uuid) = &oauth.org_uuid {
                println!("org uuid:     {uuid}");
            }
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
        println!("  Set the BALANZE_OPENAI_KEY env var to a `sk-admin-…` admin key, or run");
        println!("  `balanze set-openai-key` (note: keychain backend currently unreliable on");
        println!("  Windows; env var is the recommended path until v0.2).");
        println!("  Create an admin key at https://platform.openai.com/settings/organization/admin-keys");
    }

    // Claude Code JSONL activity
    println!();
    if let Some(jsonl) = &snapshot.claude_jsonl {
        println!("CLAUDE CODE ACTIVITY (last 5h, from local JSONL):");
        println!("  files scanned:     {}", jsonl.files_scanned);
        println!("  events in window:  {}", jsonl.window.total_events_in_window);
        println!(
            "  tokens in window:  {}",
            fmt_int(jsonl.window.total_tokens_in_window)
        );
        match jsonl.window.recent_burn_tokens_per_min {
            Some(rate) => println!(
                "  recent burn:       ~{} tokens/min (last 30 min)",
                fmt_int(rate as u64)
            ),
            None => println!("  recent burn:       (too few events in last 30 min)"),
        }
        if !jsonl.window.by_model.is_empty() {
            println!();
            println!("  By model:");
            for m in &jsonl.window.by_model {
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
