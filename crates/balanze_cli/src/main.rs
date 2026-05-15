//! Balanze CLI — composes the backend crates into a single status view.
//!
//! Subcommands:
//!   balanze-cli                      Print pretty status (default)
//!   balanze-cli status [--json]       Same as above; --json is machine-readable
//!   balanze-cli setup                 Interactive wizard: check Anthropic OAuth + Codex + OpenAI key
//!   balanze-cli set-openai-key        Read sk-... from stdin, store in OS keychain (non-interactive)
//!   balanze-cli clear-openai-key      Remove the OpenAI key from the keychain
//!   balanze-cli settings              Print current settings.json contents
//!   balanze-cli help                  This help
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
use claude_parser::{
    dedup_events, find_claude_projects_dir, find_jsonl_files, parse_str, UsageEvent,
};
use openai_client::{
    costs_this_month, OpenAiCosts, OpenAiError, DEFAULT_API_BASE as OPENAI_API_BASE,
};
use state_coordinator::{JsonlSnapshot, Snapshot};
use tracing::{info, warn};
use window::{summarize_window, DEFAULT_BURN_WINDOW, DEFAULT_MIN_BURN_EVENTS, DEFAULT_WINDOW};

/// Format an `i64` micro-USD value as a human-readable USD string. Pure
/// display path per AGENTS.md §2.1: integer math everywhere internally;
/// f64 only at the boundary.
fn micro_usd_to_display_dollars(micro: i64) -> String {
    format!("${:.2}", micro as f64 / 1_000_000.0)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("status");

    let result = match cmd {
        // `--json` and `--sections` are top-level aliases for
        // `status --json` / `status --sections`: they're peer output
        // modes, the compact view's footer + the README advertise the
        // bare form, and cmd_status already inspects the full argv (and
        // applies the documented --json-wins precedence) regardless of
        // which token routed here. `-v` is intentionally NOT an alias —
        // it's a modifier on a mode, never advertised standalone.
        "status" | "--json" | "--sections" => cmd_status(&args),
        "setup" => cmd_setup(),
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
    let sections = args.iter().any(|a| a == "--sections");
    let snapshot = tokio::runtime::Runtime::new()?.block_on(build_snapshot());

    // Precedence (documented in `balanze-cli help`): --json wins over
    // --sections if both are passed. --json is the scripting/machine
    // path; if a caller asked for it, honor it even alongside a stray
    // --sections. Not an error — silently ignoring --sections here is
    // the least-surprising behavior for `balanze-cli status --json --sections`.
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else if sections {
        // Per-source detailed view — useful for debugging, dev work, and
        // anyone who wants the full window math + cadence bars in one go.
        print_sections(&snapshot, verbose);
    } else {
        // Default: glanceable 4-quadrant matrix mirroring the readiness
        // summary from `balanze-cli setup`. Run `balanze-cli --sections` for the
        // extended per-source breakdown.
        print_compact(&snapshot);
    }
    Ok(())
}

fn cmd_set_openai_key() -> Result<()> {
    // Accept the key as a positional argument (`balanze-cli set-openai-key sk-…`)
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
                "stdin closed without input. Tip: pass the key as an argument instead — `balanze-cli set-openai-key sk-...`"
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
        eprintln!("Heads up: this doesn't look like an admin key. The organization/costs");
        eprintln!("endpoint Balanze uses requires an admin key (`sk-admin-…`); project keys");
        eprintln!("(`sk-proj-…`) and service-account keys will return 403 here. Create an");
        eprintln!("admin key at https://platform.openai.com/settings/organization/admin-keys");
        eprintln!("and replace this one if the next `balanze-cli` run shows an error.");
    }

    keychain::set(keychain::keys::OPENAI_API_KEY, &key)?;

    let mut s = settings::load().unwrap_or_default();
    s.providers.openai_enabled = true;
    settings::save(&s)?;

    eprintln!(
        "Stored OpenAI key in the OS keychain ({} bytes).",
        key.len()
    );
    if is_admin_key {
        eprintln!("Run `balanze-cli` to verify the tile shows spend data.");
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

// ────────────────────────────────────────────────────────────────────
// `balanze-cli setup` — interactive auth wizard.
//
// Flow:
//   [1/4] Check Anthropic OAuth credentials file presence.
//   [2/4] Check Codex sessions presence (codex_local).
//   [3/4] Prompt for OpenAI admin key (masked input via rpassword),
//         validate live against /v1/organization/costs, store in
//         keychain, verify the keychain write took.
//   [4/4] Print a 4-row readiness summary matching the eventual
//         `balanze-cli` output layout.
//
// Design decisions (recorded for future maintainers):
//   - Live-validate before storing: catches typos at setup time
//     rather than at first `balanze-cli` run. One network call to OpenAI.
//   - No "setup complete" marker in settings.json: the CLI infers
//     readiness from the keychain + file presence. Idempotent setup.
//   - Windows keychain bug detection: keyring v3 silently no-ops on
//     Windows; we write then read back to detect, then point the user
//     at BALANZE_OPENAI_KEY as the workaround.
//   - Existing key handling: if a key is already saved, validate it
//     (don't re-prompt). User can answer 'y' to replace.
// ────────────────────────────────────────────────────────────────────

// Status enums only carry the discriminants — paths and error messages
// are already eprintln'd at the moment they're known. If a future step
// (balanze_cli wiring) needs to thread the paths into a Snapshot, add
// the payload then. YAGNI for now.

#[derive(Debug)]
enum AnthropicOAuthStatus {
    Found,
    NotFound,
}

#[derive(Debug)]
enum CodexStatus {
    HasSessions,
    InstalledNoSessions,
    NotInstalled,
    /// Sessions dir is present but we couldn't read it (permission
    /// denied, disk I/O failure, etc.). Distinct from `NotInstalled`
    /// so the readiness summary doesn't lie about which problem the
    /// user is hitting. The specific error was already eprintln'd at
    /// step 2; this variant just lets the summary echo a truthful
    /// label.
    Error,
}

#[derive(Debug)]
enum OpenAiKeyStatus {
    SavedAndValidated,
    KeptExistingKey,
    EnvVarOverride,
    ValidationFailed,
    KeychainBroken,
}

fn cmd_setup() -> Result<()> {
    eprintln!("Balanze setup");
    eprintln!("=============");
    eprintln!();
    eprintln!("This wizard:");
    eprintln!("  1. Checks your Anthropic OAuth credentials (~/.claude/.credentials.json).");
    eprintln!("  2. Checks your Codex sessions (~/.codex/sessions/).");
    eprintln!("  3. Prompts for your OpenAI admin key, validates it live, stores it.");
    eprintln!("  4. Prints a readiness summary for all four data sources.");
    eprintln!();

    eprintln!("[1/4] Anthropic OAuth credentials");
    let anthropic = check_anthropic_oauth();
    eprintln!();

    eprintln!("[2/4] Codex CLI sessions");
    let codex = check_codex();
    eprintln!();

    eprintln!("[3/4] OpenAI admin key");
    let openai = setup_openai_key()?;
    eprintln!();

    eprintln!("[4/4] Readiness summary");
    print_readiness(&anthropic, &codex, &openai);

    Ok(())
}

fn check_anthropic_oauth() -> AnthropicOAuthStatus {
    match anthropic_oauth::locate_credentials() {
        Ok(path) => {
            eprintln!("  ✓ Found at {}", path.display());
            AnthropicOAuthStatus::Found
        }
        Err(_) => {
            eprintln!("  ✗ Not found.");
            eprintln!("    To enable: run `claude login` (writes ~/.claude/.credentials.json).");
            eprintln!("    Balanze still derives Claude API cost from JSONL session files");
            eprintln!("    without this, but the subscription-quota cell will be empty.");
            AnthropicOAuthStatus::NotFound
        }
    }
}

fn check_codex() -> CodexStatus {
    match codex_local::find_codex_sessions_dir() {
        Err(codex_local::ParseError::FileMissing(_)) => {
            eprintln!("  ✗ Codex CLI not installed (no ~/.codex/sessions/ directory).");
            eprintln!("    The Codex quota cell will be empty.");
            CodexStatus::NotInstalled
        }
        Err(e) => {
            eprintln!("  ✗ Error finding Codex sessions dir: {e}");
            CodexStatus::Error
        }
        Ok(dir) => match codex_local::find_latest_session(&dir) {
            Ok(Some(path)) => {
                eprintln!("  ✓ Latest session: {}", path.display());
                CodexStatus::HasSessions
            }
            Ok(None) => {
                eprintln!("  ○ Codex installed but no sessions yet.");
                eprintln!(
                    "    Run `codex` once to populate {} with a session file.",
                    dir.display()
                );
                CodexStatus::InstalledNoSessions
            }
            Err(e) => {
                eprintln!("  ✗ Error walking Codex sessions: {e}");
                CodexStatus::Error
            }
        },
    }
}

fn setup_openai_key() -> Result<OpenAiKeyStatus> {
    // Env-var override takes precedence over keychain everywhere in the
    // CLI; honor that here too. Validate without writing to keychain.
    if let Ok(env_key) = env::var("BALANZE_OPENAI_KEY") {
        let trimmed = env_key.trim();
        if !trimmed.is_empty() {
            eprintln!("  BALANZE_OPENAI_KEY env var is set; validating without keychain write.");
            return Ok(match validate_openai_key_blocking(trimmed) {
                Ok(()) => {
                    eprintln!("  ✓ Env-var key validated against OpenAI Admin Costs API.");
                    OpenAiKeyStatus::EnvVarOverride
                }
                Err(e) => {
                    eprintln!("  ✗ Env-var key rejected by OpenAI: {e}");
                    OpenAiKeyStatus::ValidationFailed
                }
            });
        }
    }

    // Single `keychain::get` instead of `exists` + `get` — `exists` is
    // implemented as `get(...).is_ok_or_not_found()` under the hood
    // (see `keychain::exists`), so an `exists`+`get` sequence is two
    // keychain reads. On macOS that's two ACL prompts.
    let existing_key = match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(k) => Some(k),
        Err(keychain::KeychainError::NotFound(_)) => None,
        Err(e) => return Err(e.into()),
    };
    let key = if let Some(existing_key) = existing_key {
        eprintln!("  An OpenAI key is already saved in the keychain.");
        eprint!("  Replace it? [y/N]: ");
        let _ = io::stderr().flush();
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            prompt_for_openai_key()?
        } else {
            // Keep + validate the already-loaded value. No second
            // keychain hit; no second ACL prompt on macOS.
            eprintln!("  Keeping existing key; validating against OpenAI Admin Costs API...");
            return Ok(match validate_openai_key_blocking(&existing_key) {
                Ok(()) => {
                    eprintln!("  ✓ Existing key still works.");
                    OpenAiKeyStatus::KeptExistingKey
                }
                Err(e) => {
                    eprintln!("  ✗ Existing key rejected: {e}");
                    eprintln!("    Re-run `balanze-cli setup` and choose to replace.");
                    OpenAiKeyStatus::ValidationFailed
                }
            });
        }
    } else {
        prompt_for_openai_key()?
    };

    eprintln!("  Validating against OpenAI Admin Costs API...");
    if let Err(e) = validate_openai_key_blocking(&key) {
        eprintln!("  ✗ {e}");
        eprintln!("    Key NOT saved. Re-run `balanze-cli setup` with a working key.");
        return Ok(OpenAiKeyStatus::ValidationFailed);
    }

    // Write to keychain, then read back to detect the known `keyring`
    // v3 Windows silent-no-op bug. set→get→compare exposes the bug as
    // an Err(NotFound) or value mismatch on read.
    keychain::set(keychain::keys::OPENAI_API_KEY, &key)?;
    let read_back = match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ✗ Keychain write didn't persist (read-back failed: {e}).");
            eprintln!("    Known `keyring` v3 issue on Windows. Workaround:");
            eprintln!("      export BALANZE_OPENAI_KEY=sk-admin-...   (Unix shells)");
            eprintln!("      $env:BALANZE_OPENAI_KEY = 'sk-admin-...' (PowerShell)");
            eprintln!("    The CLI honors this env var with precedence over the keychain.");
            return Ok(OpenAiKeyStatus::KeychainBroken);
        }
    };
    if read_back != key {
        eprintln!("  ✗ Keychain write didn't persist (read-back value mismatch).");
        eprintln!("    Known `keyring` v3 issue. Workaround: use BALANZE_OPENAI_KEY env var.");
        return Ok(OpenAiKeyStatus::KeychainBroken);
    }

    // Mirror `cmd_set_openai_key`'s pattern: load-or-default (corrupt
    // settings.json shouldn't block the setup wizard), but save errors
    // propagate loudly. A silent save failure here would leave
    // `settings.providers.openai_enabled = false` while the key IS in
    // the keychain — exactly the kind of desync that makes "why doesn't
    // it show up in `balanze-cli status`?" debugging painful.
    let mut s = settings::load().unwrap_or_default();
    s.providers.openai_enabled = true;
    settings::save(&s)?;
    eprintln!("  ✓ Key validated and saved to the OS keychain.");
    Ok(OpenAiKeyStatus::SavedAndValidated)
}

fn prompt_for_openai_key() -> Result<String> {
    eprintln!("  Paste your OpenAI admin key (sk-admin-...) and press Enter.");
    eprintln!("  Input is hidden; nothing will echo to the terminal.");
    let raw = rpassword::prompt_password("  Key: ")
        .map_err(|e| anyhow!("failed to read key from stdin: {e}"))?;
    let key = raw.trim().to_string();
    if key.is_empty() {
        anyhow::bail!("no key provided; aborting setup");
    }
    if !key.starts_with("sk-") {
        anyhow::bail!("key doesn't look like an OpenAI key (expected sk-...); aborting");
    }
    if !key.starts_with("sk-admin-") {
        eprintln!("  ⚠ Heads up: this isn't an admin key (sk-admin-...). The");
        eprintln!("    /v1/organization/costs endpoint Balanze uses requires admin keys;");
        eprintln!("    project (sk-proj-) and service-account keys get HTTP 403. Live");
        eprintln!("    validation will tell you for sure.");
    }
    Ok(key)
}

fn validate_openai_key_blocking(key: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .user_agent("balanze-cli/0.1.0")
            .build()?;
        match costs_this_month(&client, OPENAI_API_BASE, key).await {
            Ok(_) => Ok(()),
            Err(OpenAiError::AuthInvalid { body }) => {
                Err(anyhow!("OpenAI rejected the key (HTTP 401). Body: {body}"))
            }
            Err(OpenAiError::InsufficientScope { .. }) => Err(anyhow!(
                "OpenAI returned 403 — this key lacks admin scope. \
                 Generate an admin key at \
                 https://platform.openai.com/settings/organization/admin-keys"
            )),
            Err(e) => Err(anyhow!("OpenAI request failed: {e}")),
        }
    })
}

fn print_readiness(
    anthropic: &AnthropicOAuthStatus,
    codex: &CodexStatus,
    openai: &OpenAiKeyStatus,
) {
    let anthropic_quota = match anthropic {
        AnthropicOAuthStatus::Found => "✓ ready (anthropic_oauth)",
        AnthropicOAuthStatus::NotFound => "✗ not configured — run `claude login`",
    };
    // Anthropic API $ derivation only needs JSONL files; OAuth isn't required.
    let claude_cost = if claude_parser::find_claude_projects_dir().is_ok() {
        "✓ ready (claude_cost — estimated from JSONL)"
    } else {
        "✗ no Claude Code JSONL found"
    };
    let codex_str = match codex {
        CodexStatus::HasSessions => "✓ ready (codex_local)",
        CodexStatus::InstalledNoSessions => "○ installed, no sessions yet — run `codex` once",
        CodexStatus::NotInstalled => "✗ Codex CLI not installed",
        CodexStatus::Error => "✗ error reading Codex sessions (see message above)",
    };
    let openai_str = match openai {
        OpenAiKeyStatus::SavedAndValidated | OpenAiKeyStatus::KeptExistingKey => {
            "✓ ready (openai_client)"
        }
        OpenAiKeyStatus::EnvVarOverride => "✓ ready (via BALANZE_OPENAI_KEY env var)",
        OpenAiKeyStatus::ValidationFailed => "✗ key validation failed — re-run setup",
        OpenAiKeyStatus::KeychainBroken => "✗ keychain broken — use BALANZE_OPENAI_KEY env var",
    };

    eprintln!();
    eprintln!("  Source                       Status");
    eprintln!("  ───────────────────────────  ───────────────────────────────────────");
    eprintln!("  Anthropic subscription %     {anthropic_quota}");
    eprintln!("  Anthropic API $ (estimated)  {claude_cost}");
    eprintln!("  OpenAI Codex %               {codex_str}");
    eprintln!("  OpenAI API $                 {openai_str}");
    eprintln!();
    eprintln!("Run `balanze-cli` to see the live snapshot.");
}

// ────────────────────────────────────────────────────────────────────

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
    eprintln!("  balanze-cli                      Print 4-quadrant compact status (default)");
    eprintln!("  balanze-cli status [--json] [--sections] [-v]");
    eprintln!("                                Same as above. Flags:");
    eprintln!("                                  --sections   per-source detailed view");
    eprintln!("                                               (cadence bars, model breakdown,");
    eprintln!("                                               codex window, etc.)");
    eprintln!("                                  --json       machine-readable Snapshot JSON.");
    eprintln!("                                               Takes precedence over --sections");
    eprintln!("                                               if both are given.");
    eprintln!("                                  -v/--verbose adds account-identifying fields");
    eprintln!("                                               (org uuid, codex session_id)");
    eprintln!("                                               — safe at home, dox-y in public.");
    eprintln!("  balanze-cli setup                 Interactive wizard. Checks Anthropic OAuth,");
    eprintln!(
        "                                Codex sessions, prompts for OpenAI admin key (masked"
    );
    eprintln!(
        "                                input), validates it live, stores it. Run this first."
    );
    eprintln!(
        "  balanze-cli set-openai-key [KEY]  Non-interactive: stores KEY in the OS keychain."
    );
    eprintln!("                                Reads from stdin if KEY is omitted.");
    eprintln!("  balanze-cli clear-openai-key      Remove the OpenAI key from the keychain");
    eprintln!("  balanze-cli settings              Print current settings.json contents");
    eprintln!("  balanze-cli help                  This help");
    eprintln!();
    eprintln!("Environment overrides:");
    eprintln!(
        "  BALANZE_OPENAI_KEY            sk-admin-… admin key. Takes precedence over keychain."
    );
    eprintln!(
        "                                Recommended on Windows until the keychain backend is"
    );
    eprintln!("                                migrated to keyring v4 in v0.3.");
    eprintln!();
    eprintln!("Tip: run via `cargo run --release -p balanze_cli -- <subcommand>` (note the `--`).");
}

// TODO(v0.2): this function is the source-orchestration policy — per-source
// fetch, error→string mapping, the "JSONL load fails ⇒ both claude_jsonl and
// anthropic_api_cost stay None without duplicating the error" rule, and the
// "Codex not installed ⇒ Ok(None), not an error" rule. When `src-tauri` lands
// its pollers feeding `state_coordinator`, this exact policy will be
// reimplemented on the poller side and the two entry-points can silently
// diverge, violating the AGENTS.md §4 #8 parity contract ("identical inputs ⇒
// identical Snapshot"). The fix is to extract this orchestration into a shared
// crate (or a `state_coordinator` compose fn) that both `balanze_cli` and the
// pollers call. NOT done now on purpose: the second consumer (pollers) does
// not exist yet, so extracting today is YAGNI — the marker exists so the
// extraction happens WITH the pollers, not after a divergence bug.
async fn build_snapshot() -> Snapshot {
    let now = Utc::now();

    let (claude_oauth, claude_oauth_error) = match fetch_oauth().await {
        Ok(s) => (Some(s), None),
        Err(e) => {
            warn!("OAuth source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    // JSONL events power BOTH the window summary (claude_jsonl) and the
    // API-rate cost synthesis (anthropic_api_cost). Read once, summarize
    // twice. If the load fails entirely, both downstream slots stay None
    // and claude_jsonl_error carries the reason — we don't duplicate the
    // error into anthropic_api_cost_error (the renderer correlates).
    let mut claude_jsonl: Option<JsonlSnapshot> = None;
    let mut claude_jsonl_error: Option<String> = None;
    let mut anthropic_api_cost: Option<claude_cost::Cost> = None;
    let mut anthropic_api_cost_error: Option<String> = None;
    match load_and_dedup_claude_events() {
        Ok((events, files_scanned)) => {
            claude_jsonl = Some(summarize_for_jsonl_snapshot(&events, files_scanned, now));
            match compute_anthropic_api_cost(&events) {
                Ok(cost) => {
                    info!(
                        "claude_cost: total_micro_usd={} per_model_rows={} skipped={}",
                        cost.total_micro_usd,
                        cost.per_model.len(),
                        cost.skipped_models.len()
                    );
                    anthropic_api_cost = Some(cost);
                }
                Err(e) => {
                    warn!("anthropic_api_cost source failed: {e}");
                    anthropic_api_cost_error = Some(e.to_string());
                }
            }
        }
        Err(e) => {
            warn!("JSONL source failed: {e}");
            claude_jsonl_error = Some(e.to_string());
        }
    }

    let (codex_quota, codex_quota_error) = match fetch_codex_quota() {
        Ok(snap) => (snap, None),
        Err(e) => {
            warn!("codex_quota source failed: {e}");
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
        anthropic_api_cost,
        anthropic_api_cost_error,
        codex_quota,
        codex_quota_error,
        openai,
        openai_error,
    }
}

/// Load + dedup all UsageEvents from `~/.claude/projects/`. Shared input
/// for both the window summary and the claude_cost synthesis — we don't
/// want to walk + parse 491 JSONL files twice per `balanze-cli` invocation.
///
/// Returns `(events, files_scanned)`. Files that fail to read or parse
/// are logged (warn level) but don't fail the whole call — matches the
/// existing tolerant policy.
fn load_and_dedup_claude_events() -> Result<(Vec<UsageEvent>, usize)> {
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

    Ok((all_events, files.len()))
}

fn summarize_for_jsonl_snapshot(
    events: &[UsageEvent],
    files_scanned: usize,
    now: DateTime<Utc>,
) -> JsonlSnapshot {
    let window = summarize_window(
        events,
        now,
        DEFAULT_WINDOW,
        DEFAULT_BURN_WINDOW,
        DEFAULT_MIN_BURN_EVENTS,
    );
    JsonlSnapshot {
        files_scanned,
        window,
    }
}

fn compute_anthropic_api_cost(events: &[UsageEvent]) -> Result<claude_cost::Cost> {
    let prices = claude_cost::load_bundled_prices()
        .map_err(|e| anyhow!("claude_cost: bundled price table failed to load: {e}"))?;
    Ok(claude_cost::compute_cost(events, &prices))
}

/// Read the latest Codex rate-limit snapshot. Treats "Codex not installed"
/// as `Ok(None)` (not a failure — just an unconfigured source); only
/// surfaces actual errors (permission denied, schema drift, etc.).
fn fetch_codex_quota() -> Result<Option<codex_local::CodexQuotaSnapshot>> {
    match codex_local::read_codex_quota() {
        Ok(snap) => {
            if let Some(ref s) = snap {
                info!(
                    "codex_quota: used_percent={} plan_type={} rate_limit_reached={}",
                    s.primary.used_percent, s.plan_type, s.rate_limit_reached
                );
            } else {
                info!("codex_quota: no session data yet");
            }
            Ok(snap)
        }
        Err(codex_local::ParseError::FileMissing(_)) => Ok(None),
        Err(e) => Err(e.into()),
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
            "OpenAI admin key rejected (HTTP 401). Run `balanze-cli set-openai-key` with a fresh `sk-admin-…` key."
        )),
        Err(OpenAiError::InsufficientScope { .. }) => Err(anyhow!(
            "OpenAI returned 403. organization/costs requires an admin API key (`sk-admin-…`), not a project or service-account key. Generate one at https://platform.openai.com/settings/organization/admin-keys."
        )),
        Err(e) => Err(e.into()),
    }
}

// Legacy `build_jsonl_summary` removed in step 5; superseded by
// `load_and_dedup_claude_events` + `summarize_for_jsonl_snapshot` so the
// JSONL parse output can be reused by both `summarize_window` (jsonl
// snapshot) and `claude_cost::compute_cost` (anthropic_api_cost) without
// scanning the directory twice.

fn print_sections(snapshot: &Snapshot, verbose: bool) {
    let _ = io::stdout().flush();
    println!("=== Balanze Status ===");
    println!(
        "fetched: {}",
        snapshot.fetched_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
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
        let suffix = if costs.truncated {
            "  (partial; more pages available)"
        } else {
            ""
        };
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
        println!("  `balanze-cli set-openai-key` (note: keychain backend currently unreliable on");
        println!("  Windows; env var is the recommended path until v0.3).");
        println!(
            "  Create an admin key at https://platform.openai.com/settings/organization/admin-keys"
        );
    }

    // Claude Code JSONL activity
    println!();
    if let Some(jsonl) = &snapshot.claude_jsonl {
        println!("CLAUDE CODE ACTIVITY (last 5h, from local JSONL):");
        println!("  files scanned:     {}", jsonl.files_scanned);
        println!(
            "  events in window:  {}",
            jsonl.window.total_events_in_window
        );
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

    // Anthropic API cost (estimated, JSONL-derived via claude_cost).
    println!();
    if let Some(cost) = &snapshot.anthropic_api_cost {
        println!(
            "ANTHROPIC API COST (estimated, JSONL × LiteLLM prices @ {} / {}):",
            claude_cost::PRICE_TABLE_COMMIT,
            claude_cost::PRICE_TABLE_DATE,
        );
        println!(
            "  Total:             {} (subscription leverage — not actual spend on Pro/Max)",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        );
        println!("  Events processed:  {}", cost.total_event_count);
        if cost.unparsed_event_count > 0 {
            println!(
                "  Unparsed events:   {} (JSONL line lacked model field)",
                cost.unparsed_event_count
            );
        }
        if !cost.per_model.is_empty() {
            println!();
            println!("  By model (top 10 by spend):");
            for m in cost.per_model.iter().take(10) {
                println!(
                    "    {:36}  events: {:>4}  {}",
                    m.model,
                    m.event_count,
                    micro_usd_to_display_dollars(m.total_micro_usd)
                );
            }
            if cost.per_model.len() > 10 {
                println!("    … ({} more)", cost.per_model.len() - 10);
            }
        }
        if !cost.skipped_models.is_empty() {
            println!();
            println!("  Skipped models (in JSONL but absent from price table):");
            for name in &cost.skipped_models {
                println!("    {name}");
            }
        }
    } else if let Some(err) = &snapshot.anthropic_api_cost_error {
        println!("ANTHROPIC API COST: unavailable — {err}");
    } else if snapshot.claude_jsonl_error.is_some() {
        // No separate cost error; the underlying JSONL load failed
        // (already reported above).
        println!("ANTHROPIC API COST: unavailable — JSONL load failed (see above).");
    }

    // OpenAI Codex CLI rate-limit snapshot (from codex_local).
    println!();
    if let Some(q) = &snapshot.codex_quota {
        println!(
            "OPENAI CODEX QUOTA (plan: {}, observed {}):",
            q.plan_type,
            q.observed_at.format("%Y-%m-%d %H:%M:%S UTC"),
        );
        let resets_in = q
            .primary
            .resets_at
            .signed_duration_since(snapshot.fetched_at);
        println!(
            "  Primary window:    {:.2}% of {} minutes  (resets in {})",
            q.primary.used_percent,
            q.primary.window_duration_minutes,
            pretty_duration(resets_in),
        );
        if let Some(secondary) = &q.secondary {
            let s_resets = secondary
                .resets_at
                .signed_duration_since(snapshot.fetched_at);
            println!(
                "  Secondary window:  {:.2}% of {} minutes  (resets in {})",
                secondary.used_percent,
                secondary.window_duration_minutes,
                pretty_duration(s_resets),
            );
        }
        if q.rate_limit_reached {
            println!("  ⚠  Rate-limit reached — Codex CLI is currently throttling requests.");
        }
        if verbose {
            println!("  Session ID:        {}", q.session_id);
        }
    } else if let Some(err) = &snapshot.codex_quota_error {
        println!("OPENAI CODEX QUOTA: unavailable — {err}");
    } else {
        println!(
            "OPENAI CODEX QUOTA: not configured (Codex CLI not installed, or no sessions yet)."
        );
    }
}

/// Compact 4-quadrant matrix renderer — the default `balanze-cli` output.
///
/// One screen, no scrolling. The layout maps directly onto the design
/// doc's 4-quadrant matrix: rows are providers (Anthropic, OpenAI),
/// columns are cells (Quota %, API $). Cell content shows ✓ / ○ / ✗
/// plus a one-line summary. See `print_sections` for per-source depth.
fn print_compact(snapshot: &Snapshot) {
    let _ = io::stdout().flush();
    println!(
        "=== Balanze status ({}) ===",
        snapshot.fetched_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!();

    let anth_quota = compact_anthropic_quota(snapshot);
    let anth_cost = compact_anthropic_cost(snapshot);
    let openai_quota = compact_codex_quota(snapshot);
    let openai_cost = compact_openai_cost(snapshot);

    println!("                    {:38}  API $", "Quota %");
    println!("Anthropic           {anth_quota:38}  {anth_cost}");
    println!("OpenAI              {openai_quota:38}  {openai_cost}");
    println!();
    // The four cells are NOT the same kind of number. Two are live
    // server-reported utilization, one is a local estimate, one is a
    // real bill. Flattening them into a grid makes them look uniformly
    // authoritative; this legend re-establishes the confidence split so
    // a ~$4,000 estimate is never mistaken for ~$4,000 of real spend.
    println!("Quota % = live server-reported utilization. API $: Anthropic =");
    println!("estimated list-price for local Claude Code tokens (subscription");
    println!("leverage — NOT money you were billed); OpenAI = real billed spend.");
    println!();
    println!("Run `balanze-cli --sections` for per-source detail, or `balanze-cli --json` for machine-readable output.");
}

fn compact_anthropic_quota(s: &Snapshot) -> String {
    match (&s.claude_oauth, &s.claude_oauth_error) {
        (Some(oauth), _) => {
            if oauth.cadences.is_empty() {
                "✓ ready (no cadence bars reported)".to_string()
            } else {
                // First two cadences. `anthropic_oauth` pre-sorts by
                // cadence_sort_key (five_hour=0, seven_day=1, …), so the
                // common case is "5h + 7d". {:.1} (not {:.0}) so a
                // genuine 0.4% doesn't render as "0%" — indistinguishable
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
    match (&s.anthropic_api_cost, &s.anthropic_api_cost_error) {
        // JSONL loaded fine but zero billable events (fresh install, no
        // Claude Code sessions yet). Rendering "~$0.00 (estimated)"
        // would imply a real computation against data; it's actually
        // "nothing to compute". Treat it like the no-data case.
        (Some(cost), _) if cost.total_event_count == 0 => "○ no jsonl data yet".to_string(),
        (Some(cost), _) => format!(
            "~{} (est. list-price, not billed)",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        ),
        (None, Some(_)) => "✗ cost synthesis failed".to_string(),
        (None, None) if s.claude_jsonl_error.is_some() => "✗ jsonl load failed".to_string(),
        (None, None) => "○ no jsonl data".to_string(),
    }
}

fn compact_codex_quota(s: &Snapshot) -> String {
    match (&s.codex_quota, &s.codex_quota_error) {
        (Some(q), _) => {
            let days = q.primary.window_duration_minutes as f64 / 1440.0;
            // {:.1} for the same reason as the anthropic quota cell — a
            // genuine 0.4% must not collapse to "0%".
            format!(
                "✓ {:.1}% {}d (codex {})",
                q.primary.used_percent,
                days.round() as i64,
                q.plan_type
            )
        }
        (None, Some(_)) => "✗ codex_local error".to_string(),
        (None, None) => "○ not configured (codex)".to_string(),
    }
}

fn compact_openai_cost(s: &Snapshot) -> String {
    match (&s.openai, &s.openai_error) {
        (Some(costs), _) => format!("${:.2} (admin costs)", costs.total_usd),
        (None, Some(_)) => "✗ admin costs fetch failed".to_string(),
        (None, None) => "○ not configured (run `balanze-cli setup`)".to_string(),
    }
}

/// Short cadence tag for the compact view, keyed off the **stable
/// cadence `key`** (e.g. "five_hour", "seven_day_sonnet") rather than
/// the free-form `display_label`. `anthropic_oauth` documents
/// `display_label` as curated-but-free-form, so matching on it is
/// fragile; the `key` is the wire-stable identifier.
///
/// Each 7-day sub-variant gets a distinct suffix so a user on a
/// Sonnet-only or Opus-only flow doesn't see two indistinguishable
/// "7d" cells (e.g. "19% 7d, 84% 7d-son"). Unknown / internal-codename
/// cadences render "?" here on purpose — the full label is visible in
/// `--sections`; the compact row is a glance, not the source of truth.
fn short_cadence(key: &str) -> &'static str {
    match key {
        "five_hour" => "5h",
        "seven_day" => "7d",
        "seven_day_sonnet" => "7d-son",
        "seven_day_opus" => "7d-opus",
        "seven_day_oauth_apps" => "7d-apps",
        "seven_day_cowork" => "7d-cowork",
        "seven_day_omelette" => "7d-omel",
        _ => "?",
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
