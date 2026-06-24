//! Balanze CLI - composes the backend crates into a single status view.
//!
//! Subcommands:
//!   balanze-cli                      Print pretty status (default)
//!   balanze-cli status [--json]       Same as above; --json is machine-readable
//!   balanze-cli setup                 Interactive wizard: check Anthropic OAuth + Codex + OpenAI key
//!   balanze-cli set-openai-key        Masked-TTY prompt for sk-... (also accepts piped stdin); stores in OS keychain
//!   balanze-cli clear-openai-key      Remove the OpenAI key from the keychain
//!   balanze-cli settings              Print current settings.json contents
//!   balanze-cli --help                clap-generated help for every subcommand
//!
//! When the Tauri front-end lands, the same composition logic will live
//! behind the `get_snapshot` IPC command in `src-tauri`. This CLI is the
//! reference implementation and a useful dev tool in its own right.
//!
//! This file is the entry point + command dispatch only. The clap surface
//! lives in `cli`; the work lives in the sibling modules: `sources` (build
//! the Snapshot), `render` (compact/sections views), `setup` (the wizard),
//! `keys` (key storage), `statusline` (the statusLine command), `format`
//! (shared display helpers), plus `json_output`, `sinks`, and `watch_cmd`.

use std::process::ExitCode;

use anstream::ColorChoice;
use anyhow::{Result, anyhow};
use clap::Parser;

use crate::cli::{Cli, Commands, StatusArgs};

mod cli;
mod completions;
mod doctor;
mod format;
mod json_output;
mod keys;
mod present;
mod probes;
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

    // clap handles --help / --version / unknown-command (exit 2) by itself;
    // parse() prints and exits for those. Everything past here is a real
    // dispatch.
    let cli = Cli::parse();

    // Bare `balanze-cli` (no subcommand) defaults to `status` with default
    // args - kept because it is good DX and matches the advertised form.
    let command = cli
        .command
        .unwrap_or(Commands::Status(StatusArgs::default()));

    let color_choice = if cli.no_color {
        ColorChoice::Never
    } else {
        ColorChoice::Auto
    };

    let result = match command {
        Commands::Status(args) => cmd_status(&args, cli.verbose, color_choice),
        Commands::Watch(args) => {
            // verbose is not yet threaded into watch mode; `watch --json -v`
            // would need JsonlSink to accept a verbose flag so the JSONL
            // stream surfaces org_uuid / session_id. Warn so the user does not
            // quietly get redacted output and wonder why their jq filters do
            // not see the identifiers.
            // TODO: pass verbose to JsonlSink so `watch --json -v` surfaces
            //       org_uuid / codex session_id.
            if cli.verbose && args.json {
                eprintln!(
                    "warning: -v / --verbose is not yet threaded into `watch --json`; \
                     org_uuid and codex.session_id will be redacted as if -v were absent. \
                     Use `balanze-cli status --json -v` (one-shot) if you need identifiers."
                );
            }
            watch_cmd::run_watch_mode(args.json)
        }
        Commands::Setup => setup::cmd_setup(),
        Commands::SetOpenaiKey => keys::cmd_set_openai_key(),
        Commands::ClearOpenaiKey => keys::cmd_clear_openai_key(),
        Commands::Settings => cmd_settings(),
        Commands::Statusline => statusline::cmd_statusline(),
        // `doctor` computes its own process exit code (auth fail -> 3, network
        // fail -> 4, degraded-under-strict -> 5, ...; see probes::worst_exit_code),
        // so it cannot route through the uniform Ok/Err -> SUCCESS/FAILURE map
        // below. Return its ExitCode directly; a diverging `return` type-checks
        // alongside the other `Result<()>` arms. An Err from cmd_doctor (a
        // genuinely unexpected failure) maps to FAILURE with the standard print.
        Commands::Doctor(args) => {
            match doctor::cmd_doctor(&args, cli.quiet, cli.strict, cli.no_color) {
                Ok(code) => return ExitCode::from(code as u8),
                Err(e) => {
                    eprintln!("error: {e:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        // The remaining subcommands are declared in the surface so the clap
        // tree, --help, and completions are stable, but their handlers land in
        // later changes (export). Until then they return a non-zero error (via
        // the `Err` arm below) so a caller or script sees a failure rather than
        // a silent success exit.
        Commands::Export(_) => Err(anyhow!("export: not implemented in this release yet")),
        Commands::Completions(args) => completions::cmd_completions(args.shell),
        Commands::Man => completions::cmd_man(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_status(args: &StatusArgs, verbose: bool, color_choice: ColorChoice) -> Result<()> {
    let snapshot = tokio::runtime::Runtime::new()?.block_on(sources::build_snapshot());

    // Precedence (documented in --help): --json wins over --sections if both
    // are passed. --json is the scripting/machine path; if a caller asked for
    // it, honor it even alongside a stray --sections. Silently ignoring
    // --sections here is the least-surprising behavior for
    // `balanze-cli status --json --sections`.
    if args.json {
        // `--json` goes through json_output::render, not raw Snapshot serde:
        // money cells get a `{value_micro_usd, source, confidence, details}`
        // tagged DTO, and identifiers (org_uuid, codex session_id) are
        // redacted unless `-v`/`--verbose` is also set.
        println!("{}", json_output::render(&snapshot, verbose)?);
    } else if args.sections {
        // Per-source detailed view - useful for debugging, dev work, and
        // anyone who wants the full window math + cadence bars in one go.
        render::print_sections(&snapshot, verbose)?;
    } else {
        // Default: glanceable 4-quadrant matrix mirroring the readiness
        // summary from `balanze-cli setup`. Run `balanze-cli status --sections`
        // for the extended per-source breakdown. The colored path honors
        // --no-color / NO_COLOR / non-TTY via anstream's AutoStream.
        render::print_compact_colored(&snapshot, color_choice)?;
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
