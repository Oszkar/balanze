//! Balanze CLI - composes the backend crates into a single status view.
//!
//! Subcommands (see `balanze-cli --help` for full flag details):
//!   balanze-cli                      4-quadrant compact status (default, same as `status`)
//!   balanze-cli status               Compact status; --json for machine-readable, --sections for detail
//!   balanze-cli watch                Live view: streaming compact view (--json for JSONL stream)
//!   balanze-cli doctor               Diagnose each integration; exit codes: auth=3, net=4, other=1, strict-warn=5
//!   balanze-cli export               Export usage history as CSV (not yet implemented)
//!   balanze-cli completions <shell>  Print a shell completion script to stdout
//!   balanze-cli man                  Print the man page (roff) to stdout [hidden]
//!   balanze-cli setup                Interactive auth wizard
//!   balanze-cli set-openai-key       Masked-TTY prompt; stores sk-admin-... in OS keychain
//!   balanze-cli clear-openai-key     Remove the OpenAI key from the keychain
//!   balanze-cli settings             Print current settings.json contents
//!   balanze-cli statusline           Claude Code statusLine command (reads stdin) - FROZEN contract
//!
//! Global flags: --verbose / -v, --quiet, --no-color, --strict
//!
//! The same composition logic lives behind the `get_snapshot` IPC command in
//! `src-tauri`. This CLI is the reference implementation and a useful dev tool.
//!
//! This file is the entry point + command dispatch only. The clap surface
//! lives in `cli`; the work lives in the sibling modules: `sources` (build
//! the Snapshot), `render` (compact/sections views), `setup` (the wizard),
//! `keys` (key storage), `statusline` (the statusLine command), `format`
//! (shared display helpers), plus `json_output`, `sinks`, and `watch_cmd`.

use std::process::ExitCode;

use anstream::ColorChoice;
use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Commands, StatusArgs};
use crate::exit::{ExitClass, classify_snapshot};

mod cli;
mod completions;
mod doctor;
mod exit;
mod export;
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

    // clap handles --help / --version / unknown-command by itself; parse()
    // prints and exits 2 for those (ExitClass::Usage = 2 documents that
    // contract, but clap owns the exit - we never override it). Everything past
    // here is a real dispatch that classifies into an ExitClass exactly once.
    let cli = Cli::parse();

    let class = match run(&cli) {
        Ok(class) => class,
        Err(e) => {
            // anyhow boundary: print the full cause chain, then Other (1).
            eprintln!("error: {e:#}");
            ExitClass::Other
        }
    };

    // Classify once, exit once (AGENTS.md §9 / design §9). Codes are 0..=5, so
    // `ExitCode::from(u8)` carries them exactly while still running destructors
    // (no `std::process::exit`).
    ExitCode::from(class.code() as u8)
}

/// Dispatch a parsed `Cli` to its handler and return the `ExitClass` for the
/// outcome. `anyhow` errors propagate to `main`'s boundary (mapped to Other/1).
/// Bare `balanze-cli` (no subcommand) defaults to `status` - good DX and the
/// advertised compact form.
fn run(cli: &Cli) -> Result<ExitClass> {
    match &cli.command {
        None => run_status(&StatusArgs::default(), cli),
        Some(Commands::Status(args)) => run_status(args, cli),
        Some(Commands::Doctor(args)) => {
            // doctor folds its probe set into an ExitClass itself (shared
            // taxonomy via probes::worst_exit_code), honoring --quiet/--strict.
            doctor::cmd_doctor(args, cli.quiet, cli.strict, cli.no_color)
        }
        Some(Commands::Watch(args)) => {
            // verbose is not yet threaded into watch mode; `watch --json -v`
            // would need JsonlSink to accept a verbose flag so the JSONL stream
            // surfaces org_uuid / session_id. Warn so the user does not quietly
            // get redacted output and wonder why their jq filters do not see the
            // identifiers.
            // TODO: pass verbose to JsonlSink so `watch --json -v` surfaces
            //       org_uuid / codex session_id.
            if cli.verbose && args.json {
                eprintln!(
                    "warning: -v / --verbose is not yet threaded into `watch --json`; \
                     org_uuid and codex.session_id will be redacted as if -v were absent. \
                     Use `balanze-cli status --json -v` (one-shot) if you need identifiers."
                );
            }
            watch_cmd::run_watch_mode(args.json)?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::Setup) => {
            setup::cmd_setup()?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::SetOpenaiKey) => {
            keys::cmd_set_openai_key()?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::ClearOpenaiKey) => {
            keys::cmd_clear_openai_key()?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::Settings) => {
            cmd_settings()?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::Statusline) => {
            statusline::cmd_statusline()?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::Export(args)) => {
            export::cmd_export(args)?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::Completions(args)) => {
            completions::cmd_completions(args.shell)?;
            Ok(ExitClass::Ok)
        }
        Some(Commands::Man) => {
            completions::cmd_man()?;
            Ok(ExitClass::Ok)
        }
    }
}

/// Which render path `status` takes, decided purely from the flags so the
/// precedence is unit-testable (no Snapshot build, no I/O).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    /// `--json`: machine-readable; emitted even under --quiet (it is the data a
    /// scripting caller asked for).
    Json,
    /// `--sections`: detailed per-source human view.
    Sections,
    /// Default glanceable colored 4-quadrant matrix.
    Compact,
    /// `--quiet` with neither --json nor --sections: print nothing; the exit
    /// code still reflects the snapshot.
    Suppressed,
}

/// Precedence: --json wins over --sections wins over the default. --quiet
/// suppresses ONLY the default matrix - never --json (data) and never the
/// explicitly-requested --sections detail.
fn render_mode(json: bool, sections: bool, quiet: bool) -> RenderMode {
    if json {
        RenderMode::Json
    } else if sections {
        RenderMode::Sections
    } else if quiet {
        RenderMode::Suppressed
    } else {
        RenderMode::Compact
    }
}

/// Build the snapshot, render it (honoring --json / --sections / --quiet), then
/// classify it. The snapshot's error slots plus --strict decide the exit class;
/// rendering itself never changes the class.
fn run_status(args: &StatusArgs, cli: &Cli) -> Result<ExitClass> {
    let snapshot = tokio::runtime::Runtime::new()?.block_on(sources::build_snapshot());

    match render_mode(args.json, args.sections, cli.quiet) {
        RenderMode::Json => {
            // `--json` goes through json_output::render, not raw Snapshot serde:
            // money cells get a `{value_micro_usd, source, confidence, details}`
            // tagged DTO, and identifiers (org_uuid, codex session_id) are
            // redacted unless `-v`/`--verbose` is also set.
            println!("{}", json_output::render(&snapshot, cli.verbose)?);
        }
        RenderMode::Sections => {
            // Per-source detailed view - useful for debugging, dev work, and
            // anyone who wants the full window math + cadence bars in one go.
            render::print_sections(&snapshot, cli.verbose)?;
        }
        RenderMode::Compact => {
            // Default: glanceable 4-quadrant matrix mirroring the readiness
            // summary from `balanze-cli setup`. The colored path honors
            // --no-color / NO_COLOR / non-TTY via anstream's AutoStream.
            let color_choice = if cli.no_color {
                ColorChoice::Never
            } else {
                ColorChoice::Auto
            };
            render::print_compact_colored(&snapshot, color_choice)?;
        }
        RenderMode::Suppressed => {}
    }

    Ok(classify_snapshot(&snapshot, cli.strict))
}

fn cmd_settings() -> Result<()> {
    let s = settings::load()?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    let path = settings::default_path()?;
    eprintln!("(loaded from: {})", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{RenderMode, render_mode};

    #[test]
    fn render_mode_json_beats_quiet_and_sections() {
        // --json is data: it must win over --quiet and over a stray --sections.
        assert_eq!(render_mode(true, false, true), RenderMode::Json);
        assert_eq!(render_mode(true, true, true), RenderMode::Json);
    }

    #[test]
    fn render_mode_sections_prints_even_under_quiet() {
        // --sections is an explicit detail request, not the default matrix.
        assert_eq!(render_mode(false, true, true), RenderMode::Sections);
    }

    #[test]
    fn render_mode_quiet_suppresses_only_the_default_matrix() {
        assert_eq!(render_mode(false, false, true), RenderMode::Suppressed);
        assert_eq!(render_mode(false, false, false), RenderMode::Compact);
    }
}
