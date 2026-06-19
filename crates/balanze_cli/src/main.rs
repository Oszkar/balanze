//! Balanze CLI - composes the backend crates into a single status view.
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
//!
//! This file is the entry point + command dispatch only. The work lives in the
//! sibling modules: `sources` (build the Snapshot), `render` (compact/sections
//! views), `setup` (the wizard), `keys` (key storage), `statusline` (the
//! statusLine command), `format` (shared display helpers), plus `json_output`,
//! `sinks`, and `watch_cmd`.

use std::env;
use std::process::ExitCode;

use anyhow::Result;

mod format;
mod json_output;
mod keys;
mod render;
mod setup;
mod sinks;
mod sources;
mod statusline;
mod watch_cmd;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // keyring-core has no default credential store until one is registered;
    // do it once here before any keychain get/set (in this process or the
    // watcher it spawns).
    keychain::init_default_store();

    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("status");

    let result = match cmd {
        // `--json` and `--sections` are top-level aliases for
        // `status --json` / `status --sections`: they're peer output
        // modes, the compact view's footer + the README advertise the
        // bare form, and cmd_status already inspects the full argv (and
        // applies the documented --json-wins precedence) regardless of
        // which token routed here. `-v` is intentionally NOT an alias -
        // it's a modifier on a mode, never advertised standalone.
        // `--watch` is a top-level alias for `status --watch`, mirroring the
        // `--json` and `--sections` aliases. Both `balanze-cli --watch` and
        // `balanze-cli status --watch` route here.
        "status" | "--json" | "--sections" | "--watch" => cmd_status(&args),
        "setup" => setup::cmd_setup(),
        "set-openai-key" => keys::cmd_set_openai_key(),
        "clear-openai-key" => keys::cmd_clear_openai_key(),
        "settings" => cmd_settings(),
        "statusline" => statusline::cmd_statusline(),
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
    let watch_mode = args.iter().any(|a| a == "--watch");

    if watch_mode {
        // --sections describes the per-source breakdown, which isn't a TUI
        // concept; the watch mode always uses the compact view. Warn but don't
        // error - the --watch behavior is well-defined regardless.
        if sections {
            eprintln!(
                "warning: --sections has no effect with --watch \
                 (the watch TUI uses the compact view)"
            );
        }
        // verbose is not yet threaded into watch mode; --watch --json -v would
        // need JsonlSink to accept a verbose flag so the JSONL stream surfaces
        // org_uuid / session_id. Warn if the user explicitly asks for verbose
        // alongside --json so they don't quietly get redacted output and
        // wonder why their jq filters don't see the identifiers.
        // TODO: pass verbose to JsonlSink so --watch --json -v
        //       surfaces org_uuid / codex session_id.
        if verbose && json_mode {
            eprintln!(
                "warning: -v / --verbose is not yet threaded into --watch --json; \
                 org_uuid and codex.session_id will be redacted as if -v were absent. \
                 Use `balanze-cli --json -v` (one-shot) if you need identifiers."
            );
        }
        let _ = verbose;
        return watch_cmd::run_watch_mode(json_mode);
    }

    let snapshot = tokio::runtime::Runtime::new()?.block_on(sources::build_snapshot());

    // Precedence (documented in `balanze-cli help`): --json wins over
    // --sections if both are passed. --json is the scripting/machine
    // path; if a caller asked for it, honor it even alongside a stray
    // --sections. Not an error - silently ignoring --sections here is
    // the least-surprising behavior for `balanze-cli status --json --sections`.
    if json_mode {
        // `--json` goes through json_output::render, not raw Snapshot serde:
        // money cells get a `{value_micro_usd, source, confidence, details}`
        // tagged DTO, and identifiers (org_uuid, codex session_id) are
        // redacted unless `-v`/`--verbose` is also set.
        println!("{}", json_output::render(&snapshot, verbose)?);
    } else if sections {
        // Per-source detailed view - useful for debugging, dev work, and
        // anyone who wants the full window math + cadence bars in one go.
        render::print_sections(&snapshot, verbose)?;
    } else {
        // Default: glanceable 4-quadrant matrix mirroring the readiness
        // summary from `balanze-cli setup`. Run `balanze-cli --sections` for the
        // extended per-source breakdown.
        render::print_compact(&snapshot)?;
    }
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
    eprintln!("Balanze - local-first AI usage tracker.");
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
    eprintln!("                                               --sections and --json output -");
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
    eprintln!("                                statusLine command - see `balanze-cli setup`).");
    eprintln!("  balanze-cli help                  This help");
    eprintln!();
    eprintln!("Environment overrides:");
    eprintln!("  BALANZE_OPENAI_KEY            sk-admin-... admin key. Takes precedence over the");
    eprintln!(
        "                                keychain (handy for CI/headless or a locked keychain)."
    );
    eprintln!();
    eprintln!("Tip: run via `cargo run --release -p balanze_cli -- <subcommand>` (note the `--`).");
}
