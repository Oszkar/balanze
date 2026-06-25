//! clap derive surface for `balanze-cli`.
//!
//! Single source of truth for the command tree, parsed once in `main` and
//! dispatched to the existing handler modules. The bare invocation (no
//! subcommand) runs `status` with defaults - kept because it is good DX and
//! matches the compact view's advertised form.
//!
//! Frozen external contract: the `statusline` subcommand name and its stdin
//! payload contract are invoked by Claude Code's `settings.json`; do not
//! rename or restructure that variant (AGENTS.md boundary #12).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "balanze-cli",
    version,
    about = "Personal AI usage in one normalized view"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
    /// Surface account-identifying fields (org uuid, codex session_id)
    #[arg(short = 'v', long = "verbose", global = true)]
    pub verbose: bool,
    /// Suppress non-essential output
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Disable ANSI color (NO_COLOR is also honored)
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,
    /// Treat a degraded source as failure (non-zero exit)
    #[arg(long, global = true)]
    pub strict: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// 4-quadrant compact status (also the bare default)
    Status(StatusArgs),
    /// Live view: ratatui TUI on a TTY, else streaming
    Watch(WatchArgs),
    /// Diagnose each integration
    Doctor(DoctorArgs),
    /// Export usage history as CSV
    Export(ExportArgs),
    /// Print a shell completion script to stdout
    Completions(CompletionsArgs),
    /// Print the man page (roff) to stdout
    #[command(hide = true)]
    Man,
    /// Interactive setup wizard
    Setup,
    /// Store an OpenAI admin key in the OS keychain
    SetOpenaiKey,
    /// Remove the OpenAI key from the keychain
    ClearOpenaiKey,
    /// Print current settings.json contents
    Settings,
    /// Claude Code statusLine command (reads stdin) - FROZEN external contract
    Statusline,
}

#[derive(clap::Args, Default, Debug)]
pub struct StatusArgs {
    /// Machine-readable JSON (wins over --sections if both are given)
    #[arg(long)]
    pub json: bool,
    /// Per-source detailed view (cadence bars, model breakdown, codex window)
    #[arg(long)]
    pub sections: bool,
}

#[derive(clap::Args, Debug)]
pub struct WatchArgs {
    /// Stream one JSON document per line instead of the live view
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct DoctorArgs {
    /// Skip network validation of the OpenAI key
    #[arg(long)]
    pub offline: bool,
}

#[derive(clap::Args, Debug)]
pub struct ExportArgs {
    /// Write to a file instead of stdout
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_definition_is_valid() {
        // clap's own internal consistency check: duplicate flags, bad arg
        // configs, etc. panic here rather than at runtime.
        Cli::command().debug_assert();
    }

    #[test]
    fn bare_invocation_has_no_subcommand() {
        let cli = Cli::parse_from(["balanze-cli"]);
        assert!(
            cli.command.is_none(),
            "bare balanze-cli must leave command None so main can default to status"
        );
    }

    #[test]
    fn status_json_routes_to_status_with_json_flag() {
        let cli = Cli::parse_from(["balanze-cli", "status", "--json"]);
        match cli.command {
            Some(Commands::Status(args)) => {
                assert!(args.json, "--json must set StatusArgs.json");
                assert!(!args.sections, "--sections must default false");
            }
            other => panic!("expected Status, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn status_sections_routes_to_status_with_sections_flag() {
        let cli = Cli::parse_from(["balanze-cli", "status", "--sections"]);
        match cli.command {
            Some(Commands::Status(args)) => {
                assert!(args.sections, "--sections must set StatusArgs.sections");
                assert!(!args.json, "--json must default false");
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn watch_routes_to_watch_variant() {
        let cli = Cli::parse_from(["balanze-cli", "watch"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Watch(WatchArgs { json: false }))
        ));
    }

    #[test]
    fn watch_json_sets_json_flag() {
        let cli = Cli::parse_from(["balanze-cli", "watch", "--json"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Watch(WatchArgs { json: true }))
        ));
    }

    #[test]
    fn statusline_routes_to_stdin_handler_variant() {
        // The frozen external contract: Claude Code invokes this exact name.
        let cli = Cli::parse_from(["balanze-cli", "statusline"]);
        assert!(matches!(cli.command, Some(Commands::Statusline)));
    }

    #[test]
    fn set_openai_key_routes_to_its_variant() {
        // Preserves the masked-prompt / stdin handler entry point.
        let cli = Cli::parse_from(["balanze-cli", "set-openai-key"]);
        assert!(matches!(cli.command, Some(Commands::SetOpenaiKey)));
    }

    #[test]
    fn global_verbose_flag_parses_after_subcommand() {
        let cli = Cli::parse_from(["balanze-cli", "status", "-v"]);
        assert!(
            cli.verbose,
            "global -v must parse positioned after the subcommand"
        );
    }

    #[test]
    fn version_flag_is_accepted() {
        // clap exits the process on --version, so we assert the parse path
        // recognizes it as a built-in (Err with a Version-kind error) rather
        // than an unknown-arg usage error.
        let err = Cli::try_parse_from(["balanze-cli", "--version"])
            .expect_err("--version should short-circuit parsing");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    // -----------------------------------------------------------------------
    // FIX E(3): global flags + subcommand-specific arg parse tests
    // -----------------------------------------------------------------------

    #[test]
    fn global_strict_flag_parses() {
        let cli = Cli::parse_from(["balanze-cli", "--strict", "doctor"]);
        assert!(cli.strict, "--strict must set Cli.strict");
        // Verify it also works positioned after the subcommand.
        let cli2 = Cli::parse_from(["balanze-cli", "doctor", "--strict"]);
        assert!(cli2.strict, "--strict after subcommand must set Cli.strict");
    }

    #[test]
    fn global_quiet_flag_parses() {
        let cli = Cli::parse_from(["balanze-cli", "--quiet", "doctor"]);
        assert!(cli.quiet, "--quiet must set Cli.quiet");
        let cli2 = Cli::parse_from(["balanze-cli", "doctor", "--quiet"]);
        assert!(cli2.quiet, "--quiet after subcommand must set Cli.quiet");
    }

    #[test]
    fn global_no_color_flag_parses() {
        let cli = Cli::parse_from(["balanze-cli", "--no-color"]);
        assert!(cli.no_color, "--no-color must set Cli.no_color");
        let cli2 = Cli::parse_from(["balanze-cli", "status", "--no-color"]);
        assert!(
            cli2.no_color,
            "--no-color after subcommand must set Cli.no_color"
        );
    }

    #[test]
    fn doctor_offline_flag_parses() {
        let cli = Cli::parse_from(["balanze-cli", "doctor", "--offline"]);
        match cli.command {
            Some(Commands::Doctor(args)) => {
                assert!(args.offline, "--offline must set DoctorArgs.offline");
            }
            other => panic!("expected Doctor, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn completions_zsh_parses_shell_arg() {
        let cli = Cli::parse_from(["balanze-cli", "completions", "zsh"]);
        match cli.command {
            Some(Commands::Completions(args)) => {
                assert_eq!(
                    args.shell,
                    clap_complete::Shell::Zsh,
                    "completions zsh must set shell to Zsh"
                );
            }
            other => panic!("expected Completions, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn export_output_flag_parses() {
        use std::path::PathBuf;
        let cli = Cli::parse_from(["balanze-cli", "export", "-o", "/tmp/out.csv"]);
        match cli.command {
            Some(Commands::Export(args)) => {
                assert_eq!(
                    args.output,
                    Some(PathBuf::from("/tmp/out.csv")),
                    "-o must set ExportArgs.output"
                );
            }
            other => panic!("expected Export, got {:?}", other.is_some()),
        }
    }
}
