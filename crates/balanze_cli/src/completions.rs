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
/// underlying writer `expect()`s the write (it panics, does not `Err`, on a
/// write failure such as a broken pipe - clap#5993). To avoid that panic the
/// callers pass an in-memory `Vec<u8>` here (which never broken-pipes); the
/// buffer is then flushed to stdout by [`cmd_completions`] /
/// [`write_stdout_swallowing_broken_pipe`], where a closed downstream pipe is
/// treated as quiet success. The `Result` is kept for signature symmetry with
/// [`write_man`] (which genuinely `?`-propagates I/O errors via `clap_mangen`),
/// not because this path reports write errors.
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

/// Write `bytes` to `out`, treating a closed downstream pipe (BrokenPipe) as
/// quiet success - matches `render.rs::print_compact_colored` and `sinks.rs`,
/// so `balanze-cli completions bash | head` exits 0 instead of erroring. Other
/// I/O errors propagate. Generic over the sink so the BrokenPipe rule is unit
/// testable with an injected failing writer (the production caller passes the
/// stdout lock).
fn write_swallowing_broken_pipe<W: Write>(out: &mut W, bytes: &[u8]) -> Result<()> {
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Flush `bytes` to stdout via [`write_swallowing_broken_pipe`].
fn write_stdout_swallowing_broken_pipe(bytes: &[u8]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    write_swallowing_broken_pipe(&mut lock, bytes)
}

/// `completions <shell>` handler: stream the script to stdout.
///
/// Renders into a `Vec<u8>` first so `clap_complete`'s panic-on-write
/// generator can never hit a broken pipe (clap#5993); the buffer is then
/// flushed to stdout, where a closed pipe is quiet success.
pub(crate) fn cmd_completions(shell: Shell) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    write_completions(shell, &mut buf)?;
    write_stdout_swallowing_broken_pipe(&buf)
}

/// Hidden `man` handler: stream the roff to stdout.
///
/// Buffers first (consistent with [`cmd_completions`]) so a closed downstream
/// pipe is quiet success rather than a non-zero exit.
pub(crate) fn cmd_man() -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    write_man(&mut buf)?;
    write_stdout_swallowing_broken_pipe(&buf)
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

    /// A `Write` whose `write`/`flush` always fail with a chosen `ErrorKind`,
    /// so the BrokenPipe-swallowing rule can be tested without real stdout.
    struct FailingWriter {
        kind: std::io::ErrorKind,
    }

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.kind, "injected"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(self.kind, "injected"))
        }
    }

    #[test]
    fn broken_pipe_write_is_quiet_success() {
        let mut w = FailingWriter {
            kind: std::io::ErrorKind::BrokenPipe,
        };
        // A downstream `| head` closing the pipe must NOT surface as an error
        // (matches render.rs / sinks.rs); the handler exits 0.
        assert!(
            write_swallowing_broken_pipe(&mut w, b"some completion bytes").is_ok(),
            "BrokenPipe must be swallowed as Ok"
        );
    }

    #[test]
    fn non_broken_pipe_write_error_propagates() {
        let mut w = FailingWriter {
            kind: std::io::ErrorKind::Other,
        };
        // Any other I/O failure is a real error and must propagate so the
        // handler exits non-zero.
        assert!(
            write_swallowing_broken_pipe(&mut w, b"some completion bytes").is_err(),
            "a non-BrokenPipe error must propagate"
        );
    }
}
