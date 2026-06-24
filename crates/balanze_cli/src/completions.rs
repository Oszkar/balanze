//! `completions` and hidden `man` subcommand handlers.
//!
//! `completions <shell>` streams a shell completion script to stdout via
//! `clap_complete::generate`; `man` streams the man-page roff to stdout via
//! `clap_mangen`. Both derive their structure from `Cli::command()` so the
//! generated artifacts never drift from the parsed surface. The build.rs
//! renders the same artifacts into OUT_DIR for v0.5 packaging.

use std::io::Write;

use anyhow::Result;
use clap::CommandFactory;
use clap_complete::Shell;

use crate::cli::Cli;

/// Binary name the generated artifacts are keyed to. Must match the
/// `[[bin]] name` in Cargo.toml and the `#[command(name = ...)]` on `Cli`.
pub(crate) const BIN_NAME: &str = "balanze-cli";

/// Write a shell completion script for `shell` to `out`.
///
/// Infallible on the write axis: `clap_complete::generate` returns `()` and its
/// underlying writer panics (does not `Err`) on a write failure such as a broken
/// pipe, which is the clap-ecosystem norm. The `Result` is kept for signature
/// symmetry with [`write_man`] (which genuinely `?`-propagates I/O errors via
/// `clap_mangen`), not because this path reports write errors.
pub(crate) fn write_completions<W: Write>(shell: Shell, out: &mut W) -> Result<()> {
    let mut cmd = Cli::command();
    // clap_complete keys the generated script to the bin name; pass it
    // explicitly so the script is `balanze-cli`-named regardless of how the
    // process was invoked (e.g. `cargo run -- completions`).
    clap_complete::generate(shell, &mut cmd, BIN_NAME, out);
    Ok(())
}

/// Write the man-page roff to `out`.
pub(crate) fn write_man<W: Write>(out: &mut W) -> Result<()> {
    let cmd = Cli::command();
    clap_mangen::Man::new(cmd).render(out)?;
    Ok(())
}

/// `completions <shell>` handler: stream the script to stdout.
pub(crate) fn cmd_completions(shell: Shell) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    write_completions(shell, &mut lock)
}

/// Hidden `man` handler: stream the roff to stdout.
pub(crate) fn cmd_man() -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    write_man(&mut lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completions_string(shell: Shell) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_completions(shell, &mut buf).expect("completion generation ok");
        String::from_utf8(buf).expect("completion script is UTF-8")
    }

    #[test]
    fn every_supported_shell_emits_named_nonempty_script() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let out = completions_string(shell);
            assert!(!out.is_empty(), "{shell:?} completion must be non-empty");
            assert!(
                out.contains(BIN_NAME),
                "{shell:?} completion must name the binary '{BIN_NAME}', got:\n{out}"
            );
        }
    }

    #[test]
    fn man_page_is_nonempty_and_names_the_binary() {
        let mut buf: Vec<u8> = Vec::new();
        write_man(&mut buf).expect("man render ok");
        let out = String::from_utf8(buf).expect("roff is UTF-8");
        assert!(!out.is_empty(), "man page must be non-empty");
        assert!(
            out.contains(BIN_NAME),
            "man page must name the binary '{BIN_NAME}', got:\n{out}"
        );
    }
}
