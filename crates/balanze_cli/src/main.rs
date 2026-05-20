//! Balanze CLI — composes the backend crates into a single status view.
//!
//! Subcommands:
//!   balanze-cli                      Print pretty status (default)
//!   balanze-cli status [--json]       Same as above; --json is machine-readable
//!   balanze-cli setup                 Interactive wizard: check Anthropic OAuth + Codex + OpenAI key
//!   balanze-cli set-openai-key        Masked-TTY prompt for sk-... (also accepts piped stdin); stores in OS keychain
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

mod json_output;

use anthropic_oauth::{
    fetch_usage, load_from as load_credentials_from, locate_credentials, refresh_access_token,
    write_back, ClaudeOAuthSnapshot, CredentialsClaudeAiOauth, OAuthError, WriteBack,
    CLAUDE_CODE_CLIENT_ID, CLAUDE_CODE_TOKEN_URL, DEFAULT_API_BASE as ANTHROPIC_API_BASE,
};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use claude_parser::{
    dedup_events, find_claude_projects_dir, find_jsonl_files, parse_str, UsageEvent,
};
use openai_client::{
    costs_this_month, OpenAiCosts, OpenAiError, DEFAULT_API_BASE as OPENAI_API_BASE,
};
use state_coordinator::Snapshot;
use tracing::{info, warn};

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
        "statusline" => cmd_statusline(),
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
        // `--json` goes through json_output::render, not raw Snapshot serde:
        // money cells get a `{value_micro_usd, source, confidence, details}`
        // tagged DTO, and identifiers (org_uuid, codex session_id) are
        // redacted unless `-v`/`--verbose` is also set.
        println!("{}", json_output::render(&snapshot, verbose)?);
    } else if sections {
        // Per-source detailed view — useful for debugging, dev work, and
        // anyone who wants the full window math + cadence bars in one go.
        print_sections(&snapshot, verbose)?;
    } else {
        // Default: glanceable 4-quadrant matrix mirroring the readiness
        // summary from `balanze-cli setup`. Run `balanze-cli --sections` for the
        // extended per-source breakdown.
        print_compact(&snapshot)?;
    }
    Ok(())
}

fn cmd_set_openai_key() -> Result<()> {
    // Two input paths, by TTY status — never argv. A positional `sk-…` would
    // land in shell history / `ps`, which is the exact thing this command
    // exists to avoid:
    //   - Interactive TTY → masked input via rpassword (same pattern as
    //     `cmd_setup`'s prompt_for_openai_key).
    //   - Non-TTY (`echo $KEY | balanze-cli set-openai-key`) → read whole
    //     stdin to EOF. The pipe closes; no hang.
    use std::io::{IsTerminal, Read};

    let raw = if io::stdin().is_terminal() {
        rpassword::prompt_password("Paste your OpenAI API key (sk-...) and press Enter (hidden): ")
            .map_err(|e| anyhow!("failed to read key from stdin: {e}"))?
    } else {
        let mut input = String::new();
        let n = io::stdin().read_to_string(&mut input)?;
        if n == 0 {
            return Err(anyhow!(
                "stdin closed without input. Run interactively with a TTY, or pipe the key on stdin: `echo $KEY | balanze-cli set-openai-key`"
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
//   [1/5] Check Anthropic OAuth credentials file presence.
//   [2/5] Check Codex sessions presence (codex_local).
//   [3/5] Prompt for OpenAI admin key (masked input via rpassword),
//         validate live against /v1/organization/costs, store in
//         keychain, verify the keychain write took.
//   [4/5] Offer to wire Claude Code's statusLine to balanze-cli statusline.
//   [5/5] Print a 4-row readiness summary matching the eventual
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
    eprintln!("  4. Offers to wire Claude Code's statusLine to `balanze-cli statusline`.");
    eprintln!("  5. Prints a readiness summary for all four data sources.");
    eprintln!();

    eprintln!("[1/5] Anthropic OAuth credentials");
    let anthropic = check_anthropic_oauth();
    eprintln!();

    eprintln!("[2/5] Codex CLI sessions");
    let codex = check_codex();
    eprintln!();

    eprintln!("[3/5] OpenAI admin key");
    let openai = setup_openai_key()?;
    eprintln!();

    eprintln!("[4/5] Claude Code statusLine wiring");
    setup_statusline();
    eprintln!();

    eprintln!("[5/5] Readiness summary");
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

fn setup_statusline() {
    use claude_statusline::{
        default_settings_path, locate_settings_path, read_wire_status, wire_statusline, WireStatus,
    };
    // Bare `balanze-cli` assumes it is on PATH (true after `cargo install`).
    let invocation = "balanze-cli statusline";

    let path = match locate_settings_path() {
        Ok(p) => p,
        Err(_) => default_settings_path(),
    };
    match read_wire_status(&path) {
        Ok(WireStatus::WiredToBalanze) => {
            eprintln!(
                "  ✓ Claude Code statusLine already calls balanze-cli ({}).",
                path.display()
            );
            return;
        }
        Ok(WireStatus::OccupiedBy(cmd)) => {
            eprintln!("  ○ Claude Code statusLine is already set to a different command:");
            eprintln!("      {cmd}");
            eprintln!("    Leaving it untouched. To use Balanze, set statusLine.command to");
            eprintln!("    `{invocation}` in {} yourself.", path.display());
            return;
        }
        Ok(WireStatus::Unwired) => {}
        Err(e) => {
            eprintln!(
                "  ✗ Could not read {} ({e}); skipping statusLine wiring.",
                path.display()
            );
            return;
        }
    }

    eprintln!("  Balanze can wire Claude Code's statusLine to show live 5h/7d quota.");
    eprintln!("  This will set \"statusLine\" in {} to:", path.display());
    eprintln!("      {{ \"type\": \"command\", \"command\": \"{invocation}\" }}");
    eprintln!("  (other settings preserved; reversible by editing that file).");
    eprint!("  Wire it now? [y/N]: ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut answer = String::new();
    // read_line returns Ok(0) on EOF (not Err); an IO error is also non-fatal
    // here (advisory step). Either way `answer` stays empty and falls through
    // to the "Skipped" else branch below — never writes settings.json.
    let _ = std::io::stdin().read_line(&mut answer);
    if answer.trim().eq_ignore_ascii_case("y") {
        match wire_statusline(&path, invocation) {
            Ok(()) => {
                eprintln!("  ✓ Wired. Restart Claude Code to see the Balanze status line.")
            }
            Err(e) => eprintln!("  ✗ Failed to write {} ({e}); not wired.", path.display()),
        }
    } else {
        eprintln!("  ○ Skipped (settings.json untouched).");
    }
}

fn validate_openai_key_blocking(key: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .user_agent("balanze-cli/0.1.0")
            .build()?;
        // One-shot CLI must not block on provider backoff; watcher passes standard().
        match costs_this_month(
            &client,
            OPENAI_API_BASE,
            key,
            &backoff::BackoffPolicy::fail_fast(),
        )
        .await
        {
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

fn cmd_statusline() -> Result<()> {
    use std::io::Read as _;
    let mut stdout = std::io::stdout().lock();
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        let _ = writeln!(stdout, "bal (statusline: stdin unreadable)");
        return Ok(());
    }
    let _ = writeln!(stdout, "{}", format_statusline(&buf));
    Ok(())
}

/// Pure: payload string → one status-line string. Track D = a minimal
/// honest line. Rich/configurable formatting + feeding the live Snapshot
/// is Track E (the redefined watcher).
fn format_statusline(payload: &str) -> String {
    let snap = match claude_statusline::parse(payload) {
        Ok(s) => s,
        Err(_) => return "bal (statusline parse error)".to_string(),
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(rl) = &snap.rate_limits {
        // {:.0}: a statusline is a glance — sub-1% truncation is acceptable
        // here. compact_anthropic_quota uses {:.1} to avoid the "0%" == "no
        // usage" ambiguity in the full terminal view; that concern does not
        // apply to a terse one-liner. Intentional inconsistency — do not
        // "align" these without re-reading both rationales.
        if let Some(w) = &rl.five_hour {
            parts.push(format!("5h {:.0}%", w.used_percent));
        }
        if let Some(w) = &rl.seven_day {
            parts.push(format!("7d {:.0}%", w.used_percent));
        }
    }
    if let Some(c) = snap.session_cost_micro_usd {
        // `sess-est`, not `sess`: this is a Claude-side session estimate
        // (claude_statusline/types.rs:22) — a distinct cost tier from the
        // JSONL list-price estimate and the real `extra_usage` overage.
        // The qualifier mirrors compact_anthropic_quota's `est-leverage`
        // discipline so a statusline glance can't be mistaken for billed $.
        parts.push(format!("sess-est {}", micro_usd_to_display_dollars(c)));
    }
    if parts.is_empty() {
        "bal (no rate-limit data yet)".to_string()
    } else {
        format!("bal {}", parts.join(" · "))
    }
}

#[cfg(test)]
mod statusline_tests {
    use super::format_statusline;

    #[test]
    fn formats_full_payload() {
        let p = r#"{"rate_limits":{"five_hour":{"used_percentage":13.0,"resets_at":1747650600},"seven_day":{"used_percentage":44.0,"resets_at":1747915200}},"cost":{"total_cost_usd":12.5}}"#;
        assert_eq!(
            format_statusline(p),
            "bal 5h 13% · 7d 44% · sess-est $12.50"
        );
    }
    #[test]
    fn formats_no_rate_limits() {
        assert_eq!(
            format_statusline(r#"{"cost":{"total_cost_usd":2.0}}"#),
            "bal sess-est $2.00"
        );
    }
    #[test]
    fn formats_empty_payload() {
        assert_eq!(format_statusline("{}"), "bal (no rate-limit data yet)");
    }
    #[test]
    fn parse_error_is_nonempty_fallback_not_panic() {
        assert_eq!(
            format_statusline("not json"),
            "bal (statusline parse error)"
        );
    }
    #[test]
    fn formats_only_seven_day() {
        let p = r#"{"rate_limits":{"seven_day":{"used_percentage":72.0,"resets_at":1747915200}}}"#;
        assert_eq!(format_statusline(p), "bal 7d 72%");
    }
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
    eprintln!("                                  --json       machine-readable JSON. Each money");
    eprintln!("                                               cell is {{value_micro_usd, source,");
    eprintln!("                                               confidence, details}}. Wins over");
    eprintln!("                                               --sections if both are given.");
    eprintln!("                                  -v/--verbose adds account-identifying fields");
    eprintln!(
        "                                               (org_uuid, codex session_id) to both"
    );
    eprintln!("                                               --sections and --json output —");
    eprintln!("                                               safe at home, dox-y in public.");
    eprintln!("  balanze-cli setup                 Interactive wizard. Checks Anthropic OAuth,");
    eprintln!(
        "                                Codex sessions, prompts for OpenAI admin key (masked"
    );
    eprintln!(
        "                                input), validates it live, stores it. Also offers to"
    );
    eprintln!("                                wire Claude Code's statusLine. Run this first.");
    eprintln!("  balanze-cli set-openai-key        Store an OpenAI admin key in the OS keychain.");
    eprintln!(
        "                                Interactive: masked TTY prompt (no echo, no history)."
    );
    eprintln!(
        "                                Automation: `echo $KEY | balanze-cli set-openai-key`."
    );
    eprintln!("  balanze-cli clear-openai-key      Remove the OpenAI key from the keychain");
    eprintln!("  balanze-cli settings              Print current settings.json contents");
    eprintln!("  balanze-cli statusline            Read Claude Code's statusLine JSON on stdin,");
    eprintln!("                                print a one-line status (used as Claude Code's");
    eprintln!("                                statusLine command — see `balanze-cli setup`).");
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

// The source-orchestration policy now lives in `snapshot_composer::compose`
// (AGENTS.md §4 #8): the CLI runs it via `LiveSources`, the future watcher
// will run it via its own `SnapshotSources` impl, and `integration_4quadrant`
// runs it via `FixtureSources` — one policy, no silent divergence.
async fn build_snapshot() -> Snapshot {
    snapshot_composer::compose(&LiveSources, Utc::now()).await
}

/// The production `SnapshotSources`: real network + filesystem + keychain.
/// Every method body delegates to the pre-extraction helper, moved unchanged.
struct LiveSources;

impl snapshot_composer::SnapshotSources for LiveSources {
    async fn fetch_oauth(&self) -> Result<ClaudeOAuthSnapshot> {
        live_fetch_oauth().await
    }
    async fn load_claude_events(&self) -> Result<(Vec<UsageEvent>, usize)> {
        live_load_claude_events()
    }
    async fn fetch_codex_quota(&self) -> Result<Option<codex_local::CodexQuotaSnapshot>> {
        live_fetch_codex_quota()
    }
    async fn fetch_openai(&self) -> Result<Option<OpenAiCosts>> {
        live_fetch_openai().await
    }
}

/// Load + dedup all UsageEvents from `~/.claude/projects/`. Shared input
/// for both the window summary and the claude_cost synthesis — we don't
/// want to walk + parse 491 JSONL files twice per `balanze-cli` invocation.
///
/// Returns `(events, files_scanned)`. Files that fail to read or parse
/// are logged (warn level) but don't fail the whole call — matches the
/// existing tolerant policy.
fn live_load_claude_events() -> Result<(Vec<UsageEvent>, usize)> {
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

/// Read the latest Codex rate-limit snapshot. Treats "Codex not installed"
/// as `Ok(None)` (not a failure — just an unconfigured source); only
/// surfaces actual errors (permission denied, schema drift, etc.).
fn live_fetch_codex_quota() -> Result<Option<codex_local::CodexQuotaSnapshot>> {
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

/// Refresh proactively if the access token is expired or expires within this.
const REFRESH_MARGIN: Duration = Duration::seconds(300);

/// Pure: true if `expires_at_ms` is in the past or within `margin` of now.
fn token_needs_refresh(expires_at_ms: i64, now: DateTime<Utc>, margin: Duration) -> bool {
    // Fix 5: saturating_sub so a pathological/hostile expires_at_ms near
    // i64::MIN cannot cause an arithmetic underflow panic in debug builds.
    now.timestamp_millis() >= expires_at_ms.saturating_sub(margin.num_milliseconds())
}

/// Refresh the bearer and best-effort persist it. A skipped/failed write is
/// non-fatal as long as we hold a usable token in memory.
async fn refresh_and_persist(
    client: &reqwest::Client,
    path: &std::path::Path,
    oauth: CredentialsClaudeAiOauth,
) -> Result<CredentialsClaudeAiOauth> {
    let rt = oauth
        .refresh_token
        .as_deref()
        .ok_or(OAuthError::RefreshTokenMissing)?;
    // One-shot CLI must not block on provider backoff; the watcher will pass standard().
    let refreshed = refresh_access_token(
        client,
        CLAUDE_CODE_TOKEN_URL,
        CLAUDE_CODE_CLIENT_ID,
        rt,
        Utc::now().timestamp_millis(),
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await?;
    match write_back(path, &refreshed) {
        Ok(WriteBack::Written) => info!("oauth: refreshed bearer, wrote back"),
        Ok(WriteBack::SkippedDiskNewer) => {
            info!("oauth: refreshed bearer; on-disk copy already newer, kept disk")
        }
        Err(e) => warn!("oauth: refresh ok but write-back failed (non-fatal): {e}"),
    }
    let mut next = oauth;
    next.access_token = refreshed.access_token;
    next.refresh_token = Some(refreshed.refresh_token);
    next.expires_at = refreshed.expires_at_ms;
    Ok(next)
}

async fn live_fetch_oauth() -> Result<ClaudeOAuthSnapshot> {
    let path = locate_credentials()?;
    let creds = load_credentials_from(&path)?;
    let mut oauth = creds.claude_ai_oauth;
    let client = reqwest::Client::builder()
        .user_agent("balanze-cli/0.1.0")
        .build()?;

    if token_needs_refresh(oauth.expires_at, Utc::now(), REFRESH_MARGIN) {
        info!("oauth: token expired/near-expiry — refreshing pre-flight");
        oauth = refresh_and_persist(&client, &path, oauth).await?;
    }

    // One-shot CLI must not block on provider backoff; the watcher will pass standard().
    let policy = backoff::BackoffPolicy::fail_fast();

    match fetch_usage(
        &client,
        ANTHROPIC_API_BASE,
        &oauth.access_token,
        oauth.subscription_type.clone(),
        oauth.rate_limit_tier.clone(),
        &policy,
    )
    .await
    {
        Ok(s) => {
            info!("oauth: fetched {} cadence bars", s.cadences.len());
            Ok(s)
        }
        Err(OAuthError::AuthExpired) => {
            // Note: if pre-flight already refreshed and we still 401, this does
            // one more refresh+retry. Intentional and bounded — the retry uses
            // `?`, so a second AuthExpired propagates (no loop). Do not "optimize"
            // into a did-we-already-refresh flag; KISS over a rare cold path.
            warn!("oauth: 401 despite pre-flight — one refresh+retry");
            let oauth = refresh_and_persist(&client, &path, oauth).await?;
            let s = fetch_usage(
                &client,
                ANTHROPIC_API_BASE,
                &oauth.access_token,
                oauth.subscription_type,
                oauth.rate_limit_tier,
                &policy,
            )
            .await?;
            info!(
                "oauth: fetched {} cadence bars after refresh",
                s.cadences.len()
            );
            Ok(s)
        }
        Err(e) => Err(e.into()),
    }
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
async fn live_fetch_openai() -> Result<Option<OpenAiCosts>> {
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
    // One-shot CLI must not block on provider backoff; watcher passes standard().
    match costs_this_month(&client, OPENAI_API_BASE, &key, &backoff::BackoffPolicy::fail_fast())
        .await
    {
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

fn print_sections(snapshot: &Snapshot, verbose: bool) -> io::Result<()> {
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
        // Extra-usage = pay-as-you-go overage. Resolved 2026-05-19 spike:
        // raw ints are cents; this is the claude.ai "Extra usage" meter —
        // REAL billed money, distinct from the estimated API-rate figure
        // below. Only meaningful when the user enabled it.
        if let Some(eu) = &oauth.extra_usage {
            if eu.is_enabled {
                writeln!(w)?;
                writeln!(
                    w,
                    "EXTRA USAGE (pay-as-you-go overage — REAL billed spend, from Anthropic OAuth):"
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
                    "  Real money billed beyond your subscription — NOT the estimate below."
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
        writeln!(w, "CADENCE BARS: unavailable — {err}")?;
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
        writeln!(w, "  Total: ${:.2}{suffix}", costs.total_usd)?;
        if !costs.by_line_item.is_empty() {
            writeln!(w)?;
            writeln!(w, "  By line item:")?;
            for item in costs.by_line_item.iter().take(10) {
                writeln!(w, "    {:36}  ${:>10.4}", item.line_item, item.amount_usd)?;
            }
            if costs.by_line_item.len() > 10 {
                writeln!(w, "    … ({} more)", costs.by_line_item.len() - 10)?;
            }
        }
    } else if let Some(err) = &snapshot.openai_error {
        writeln!(w, "OPENAI SPEND: unavailable — {err}")?;
    } else {
        writeln!(w, "OPENAI SPEND: not configured")?;
        writeln!(
            w,
            "  Set the BALANZE_OPENAI_KEY env var to a `sk-admin-…` admin key, or run"
        )?;
        writeln!(
            w,
            "  `balanze-cli set-openai-key` (note: keychain backend currently unreliable on"
        )?;
        writeln!(w, "  Windows; env var is the recommended path until v0.3).")?;
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
        writeln!(w, "CLAUDE CODE ACTIVITY: unavailable — {err}")?;
    }

    // Anthropic API cost (estimated, JSONL-derived via claude_cost).
    writeln!(w)?;
    if let Some(cost) = &snapshot.anthropic_api_cost {
        writeln!(
            w,
            "ANTHROPIC API COST — ESTIMATE ONLY (JSONL × LiteLLM list-price @ {} / {}):",
            claude_cost::PRICE_TABLE_COMMIT,
            claude_cost::PRICE_TABLE_DATE,
        )?;
        writeln!(
            w,
            "  Est. list-price:   {} — subscription leverage, NOT money billed",
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
                writeln!(w, "    … ({} more)", cost.per_model.len() - 10)?;
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
        writeln!(w, "ANTHROPIC API COST: unavailable — {err}")?;
    } else if snapshot.claude_jsonl_error.is_some() {
        // No separate cost error; the underlying JSONL load failed
        // (already reported above).
        writeln!(
            w,
            "ANTHROPIC API COST: unavailable — JSONL load failed (see above)."
        )?;
    }

    // OpenAI Codex CLI rate-limit snapshot (from codex_local).
    writeln!(w)?;
    if let Some(q) = &snapshot.codex_quota {
        // observed_at is the Codex CLI's own timestamp on the rate-limit
        // event. The "age" here is `fetched_at - observed_at` — how stale
        // the snapshot is relative to right-now. `codex_local::walker`
        // always returns the newest-mtime file regardless of how old it
        // is (see its docs), so a user who hasn't run Codex in a week
        // still sees data — surfacing the age lets them judge for
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
                "  ⚠  Rate-limit reached — Codex CLI is currently throttling requests."
            )?;
        }
        if verbose {
            writeln!(w, "  Session ID:        {}", q.session_id)?;
        }
    } else if let Some(err) = &snapshot.codex_quota_error {
        writeln!(w, "OPENAI CODEX QUOTA: unavailable — {err}")?;
    } else {
        writeln!(
            w,
            "OPENAI CODEX QUOTA: not configured (Codex CLI not installed, or no sessions yet)."
        )?;
    }
    Ok(())
}

/// Compact 4-quadrant matrix renderer — the default `balanze-cli` output.
///
/// One screen, no scrolling. The layout maps directly onto the design
/// doc's 4-quadrant matrix: rows are providers (Anthropic, OpenAI),
/// columns are cells (Quota %, API $). Cell content shows ✓ / ○ / ✗
/// plus a one-line summary. See `print_sections` for per-source depth.
fn print_compact(snapshot: &Snapshot) -> io::Result<()> {
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
fn write_compact<W: Write>(snapshot: &Snapshot, w: &mut W) -> io::Result<()> {
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

    writeln!(w, "                    {:38}  API $", "Quota %")?;
    writeln!(w, "Anthropic           {anth_quota:38}  {anth_cost}")?;
    writeln!(w, "OpenAI              {openai_quota:38}  {openai_cost}")?;
    writeln!(w)?;
    // The four cells are NOT the same kind of number. Two are live
    // server-reported utilization, one is a local estimate, one is a
    // real bill. Flattening them into a grid makes them look uniformly
    // authoritative; this legend re-establishes the confidence split so
    // a ~$4,000 estimate is never mistaken for ~$4,000 of real spend.
    writeln!(
        w,
        "Quota % = live server-reported utilization. API $: Anthropic ="
    )?;
    writeln!(
        w,
        "estimated list-price for local Claude Code tokens (subscription"
    )?;
    writeln!(
        w,
        "leverage — NOT billed). 'overage billed' = REAL pay-as-you-go"
    )?;
    writeln!(w, "spend from Anthropic. OpenAI = real billed spend.")?;
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
    // Real billed overage (only when the user enabled pay-as-you-go) leads;
    // the JSONL figure is ALWAYS tagged leverage-not-billed so a ~$4,000
    // estimate is never read as ~$4,000 of real spend.
    let overage = s
        .claude_oauth
        .as_ref()
        .and_then(|o| o.extra_usage.as_ref())
        .filter(|eu| eu.is_enabled)
        .map(|eu| {
            format!(
                "{}/{} overage billed",
                micro_usd_to_display_dollars(eu.used_credits_micro_usd),
                micro_usd_to_display_dollars(eu.monthly_limit_micro_usd)
            )
        });
    let est = match (&s.anthropic_api_cost, &s.anthropic_api_cost_error) {
        (Some(cost), _) if cost.total_event_count == 0 => "○ no jsonl data yet".to_string(),
        (Some(cost), _) => format!(
            "~{} est-leverage (not billed)",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        ),
        (None, Some(_)) => "✗ cost synthesis failed".to_string(),
        (None, None) if s.claude_jsonl_error.is_some() => "✗ jsonl load failed".to_string(),
        (None, None) => "○ no jsonl data".to_string(),
    };
    match overage {
        Some(o) => format!("{o} · {est}"),
        None => est,
    }
}

fn compact_codex_quota(s: &Snapshot) -> String {
    match (&s.codex_quota, &s.codex_quota_error) {
        (Some(q), _) => {
            let days = q.primary.window_duration_minutes as f64 / 1440.0;
            // Append snapshot age when meaningfully stale (≥1 min). The
            // walker returns the newest-mtime rollout file regardless of
            // age, so a 7-day-old session and a 2-min-old session look
            // identical without this tag — see `format_codex_age` doc.
            let age_tag = format_codex_age(q.observed_at, s.fetched_at)
                .map(|s| format!(", {s} old"))
                .unwrap_or_default();
            // {:.1} for the same reason as the anthropic quota cell — a
            // genuine 0.4% must not collapse to "0%".
            format!(
                "✓ {:.1}% {}d (codex {}{age_tag})",
                q.primary.used_percent,
                days.round() as i64,
                q.plan_type
            )
        }
        (None, Some(_)) => "✗ codex_local error".to_string(),
        (None, None) => "○ not configured (codex)".to_string(),
    }
}

/// Format the age of a Codex snapshot for the rendered output. Returns
/// `None` for "fresh" snapshots (< 1 min old) so the common case stays
/// noise-free; otherwise returns a tight "Nm" / "Nh" / "Nd" tag.
///
/// Negative durations (observed_at in the future, e.g. clock skew) clamp
/// to `None` — we don't want to show "−2m old" or panic on subtraction.
fn format_codex_age(observed_at: DateTime<Utc>, fetched_at: DateTime<Utc>) -> Option<String> {
    let age = fetched_at.signed_duration_since(observed_at);
    let total_secs = age.num_seconds();
    if total_secs < 60 {
        return None;
    }
    if total_secs < 3600 {
        return Some(format!("{}m", total_secs / 60));
    }
    if total_secs < 86_400 {
        return Some(format!("{}h", total_secs / 3600));
    }
    Some(format!("{}d", total_secs / 86_400))
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

#[cfg(test)]
mod tests {
    use super::*;
    use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot, ExtraUsage};
    use chrono::TimeZone;
    use claude_cost::{Cost, ModelCost};
    use codex_local::{CodexQuotaSnapshot, RateLimitWindow};
    use openai_client::{LineItemCost, OpenAiCosts};
    use state_coordinator::JsonlSnapshot;
    use window::{ByModel, WindowSummary};

    #[test]
    fn token_needs_refresh_logic() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();
        let margin = chrono::Duration::seconds(300);
        let now_ms = now.timestamp_millis();
        assert!(super::token_needs_refresh(now_ms - 1, now, margin));
        assert!(super::token_needs_refresh(now_ms + 200_000, now, margin));
        assert!(!super::token_needs_refresh(now_ms + 3_600_000, now, margin));
        // Boundary: token expiring exactly `margin` from now → refresh now.
        assert!(super::token_needs_refresh(
            now_ms + margin.num_milliseconds(),
            now,
            margin
        ));
        // Fix 5: pathological/hostile expires_at near i64::MIN must not panic
        // and must return true (absurdly-past expiry → needs refresh).
        assert!(super::token_needs_refresh(i64::MIN, now, margin));
    }

    // -----------------------------------------------------------------------
    // format_codex_age — the freshness tag wired into compact + sections
    // views. The walker returns the newest-mtime rollout file regardless of
    // age (intentional, see crate doc); the renderer surfaces age so a 7d
    // stale snapshot can be distinguished from a 2-min-old one.
    // -----------------------------------------------------------------------

    fn t(year: i32, month: u32, day: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, h, m, 0).unwrap()
    }

    #[test]
    fn format_codex_age_under_one_minute_is_silent() {
        let fetched = t(2026, 5, 20, 12, 0);
        let observed = fetched - Duration::seconds(30);
        assert_eq!(format_codex_age(observed, fetched), None);
    }

    #[test]
    fn format_codex_age_minutes_hours_days() {
        let fetched = t(2026, 5, 20, 12, 0);
        assert_eq!(
            format_codex_age(fetched - Duration::minutes(5), fetched).as_deref(),
            Some("5m")
        );
        assert_eq!(
            format_codex_age(fetched - Duration::hours(3), fetched).as_deref(),
            Some("3h")
        );
        assert_eq!(
            format_codex_age(fetched - Duration::days(8), fetched).as_deref(),
            Some("8d")
        );
    }

    #[test]
    fn format_codex_age_clamps_negative_to_none() {
        // observed_at AHEAD of fetched_at (clock skew / future-dated event).
        // Must not panic on subtraction and must not render "−5m old".
        let fetched = t(2026, 5, 20, 12, 0);
        let observed = fetched + Duration::minutes(5);
        assert_eq!(format_codex_age(observed, fetched), None);
    }

    // -----------------------------------------------------------------------
    // Label-discipline goldens for the compact + sections views. These pin
    // the four-tier confidence vocabulary the product depends on:
    //   1. ESTIMATE (JSONL × LiteLLM list price — subscription leverage)
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
    /// the estimate ($4.20) and the real overage ($20.92) are distinct — i.e.
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
            total_usd: 123.45,
            by_line_item: vec![LineItemCost {
                line_item: "gpt-5".to_string(),
                amount_usd: 123.45,
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
        let snap = fully_populated_snapshot();
        let out = render_compact(&snap);

        // Tier 1 — JSONL × list-price estimate. MUST be tagged "leverage"
        // and "not billed". Renaming to anything subtler is a regression.
        assert!(
            out.contains("est-leverage (not billed)"),
            "compact must label the JSONL × list-price figure as a leverage estimate, NOT billed\n{out}"
        );

        // Tier 2 — Real pay-as-you-go overage. MUST keep "overage billed" or
        // equivalent. Without it the user can't tell this from the estimate.
        assert!(
            out.contains("overage billed"),
            "compact must label the extra_usage block as real overage billing\n{out}"
        );

        // Tier 3a — Anthropic server quota. The "(oauth)" suffix is the
        // wire-source tag.
        assert!(
            out.contains("(oauth)"),
            "compact Anthropic-quota cell must carry the (oauth) source tag\n{out}"
        );

        // Tier 3b — Codex server quota. "(codex …" carries the source.
        assert!(
            out.contains("(codex go"),
            "compact OpenAI-quota cell must carry the (codex …) source tag\n{out}"
        );

        // Tier 4 — OpenAI real billed spend, from the Admin Costs endpoint.
        assert!(
            out.contains("(admin costs)"),
            "compact OpenAI-cost cell must carry the (admin costs) source tag\n{out}"
        );

        // Legend re-establishes the confidence split. All three phrases are
        // load-bearing — removing any of them silently downgrades the safety
        // net the legend exists to provide.
        assert!(
            out.contains("subscription"),
            "legend must mention subscription leverage:\n{out}"
        );
        assert!(
            out.contains("NOT billed"),
            "legend must include 'NOT billed' qualifier:\n{out}"
        );
        assert!(
            out.contains("REAL pay-as-you-go"),
            "legend must call out REAL pay-as-you-go spend:\n{out}"
        );
        assert!(
            out.contains("real billed spend"),
            "legend must label OpenAI as real billed spend:\n{out}"
        );

        // Codex age tag — observed_at is 5min behind fetched_at, so the
        // ", 5m old" suffix must appear. Pins the new freshness signal.
        assert!(
            out.contains(", 5m old"),
            "compact codex cell must surface the snapshot age:\n{out}"
        );
    }

    #[test]
    fn compact_view_does_not_conflate_estimate_and_real_spend() {
        let snap = fully_populated_snapshot();
        let out = render_compact(&snap);

        // Negative guards: phrases that, if they ever appear on the
        // estimate line, would obliterate the confidence split.
        assert!(
            !out.contains("est-leverage (billed)"),
            "the estimate line must never claim it is billed:\n{out}"
        );
        // The estimate $ value must not appear unqualified — every
        // appearance must sit next to the leverage tag on the same line.
        for line in out.lines() {
            if line.contains("$4.20") {
                assert!(
                    line.contains("est-leverage")
                        || line.contains("est."),
                    "every line carrying the estimate $ must carry a qualifier; offending line: {line:?}\nfull output:\n{out}"
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
            out.contains("EXTRA USAGE (pay-as-you-go overage — REAL billed spend"),
            "EXTRA USAGE section must carry the REAL billed spend qualifier:\n{out}"
        );

        // The estimate block MUST carry "ESTIMATE ONLY" and the leverage
        // disclaimer on the value line.
        assert!(
            out.contains("ANTHROPIC API COST — ESTIMATE ONLY"),
            "ANTHROPIC API COST section header must carry the ESTIMATE ONLY tag:\n{out}"
        );
        assert!(
            out.contains("subscription leverage, NOT money billed"),
            "estimate value line must carry the leverage-not-billed qualifier:\n{out}"
        );

        // Cross-link from estimate block back to extra-usage block — keeps
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

        // Codex header — plan + age. The plan_type "go" + observed_at 5min
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
        // away — but the estimate block MUST still carry its qualifier
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
            out.contains("ANTHROPIC API COST — ESTIMATE ONLY"),
            "estimate block qualifier must survive extra_usage going away:\n{out}"
        );
        assert!(
            out.contains("subscription leverage, NOT money billed"),
            "leverage qualifier must survive extra_usage going away:\n{out}"
        );
    }
}
