//! `setup` subcommand: interactive auth wizard (Anthropic OAuth check, Codex
//! check, OpenAI key prompt/validate/store, statusLine wiring, readiness
//! summary).

use anyhow::{Result, anyhow};
use std::env;
use std::io::{self, Write};

use anthropic_oauth::{load_from_source, locate_credentials};
use claude_parser::find_all_claude_projects_dirs;
use openai_client::{DEFAULT_API_BASE as OPENAI_API_BASE, OpenAiError, costs_this_month};

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
//   - Keychain write-back verification: we write then read back to
//     confirm the credential actually persisted (a locked keychain or
//     permission issue can fail the write); on failure we point the user
//     at BALANZE_OPENAI_KEY as the fallback.
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

pub(crate) fn cmd_setup() -> Result<()> {
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
    // Locate AND load: on macOS the source is the login Keychain, returned
    // optimistically by `locate_credentials`, so confirm the entry actually
    // reads before reporting it found (may prompt for Keychain access once).
    let located = locate_credentials()
        .ok()
        .filter(|src| load_from_source(src).is_ok());
    match located {
        Some(source) => {
            eprintln!("  ✓ Found at {}", source.describe());
            AnthropicOAuthStatus::Found
        }
        None => {
            eprintln!("  ✗ Not found.");
            eprintln!("    To enable: run `claude login` (writes ~/.claude/.credentials.json,");
            eprintln!("    or the login Keychain on recent macOS).");
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

    // Write to keychain, then read back to confirm the credential actually
    // persisted. set→get→compare surfaces any silent write failure (a locked
    // keychain, a permission issue) as an Err(NotFound) or value mismatch.
    keychain::set(keychain::keys::OPENAI_API_KEY, &key)?;
    let read_back = match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ✗ Keychain write didn't persist (read-back failed: {e}).");
            eprintln!("    Fallback - set the key via env var instead:");
            eprintln!("      export BALANZE_OPENAI_KEY=sk-admin-...   (Unix shells)");
            eprintln!("      $env:BALANZE_OPENAI_KEY = 'sk-admin-...' (PowerShell)");
            eprintln!("    The CLI honors this env var with precedence over the keychain.");
            return Ok(OpenAiKeyStatus::KeychainBroken);
        }
    };
    if read_back != key {
        eprintln!("  ✗ Keychain write didn't persist (read-back value mismatch).");
        eprintln!("    Fallback: set BALANZE_OPENAI_KEY env var instead.");
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
        STATUSLINE_INVOCATION, WireStatus, default_settings_path, locate_settings_path,
        read_wire_status, wire_statusline,
    };
    // Shared const so the CLI and the desktop Settings UI can't drift.
    let invocation = STATUSLINE_INVOCATION;

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
    let claude_cost = if !find_all_claude_projects_dirs().is_empty() {
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
