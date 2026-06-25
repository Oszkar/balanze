//! Build-time artifact generation: shell completions + man page into OUT_DIR
//! for packaging (installers wire these into the system completion/man dirs).
//! This is a CONVENIENCE copy. The authoritative, drift-free artifacts at
//! runtime are produced by the `completions` and `man` subcommands, which
//! render from the real `Cli::command()`.
//!
//! Guarantees (AGENTS.md §3.1): pure-Rust, no network, no system libs.
//! Any artifact-write failure degrades to a `cargo:warning` and NEVER fails
//! the CLI build - the CLI must build on Linux with only a Rust toolchain.
//!
//! A build.rs cannot import the crate it is compiling, so the command is
//! reconstructed here from a minimal mirror. Drift is acceptable: only the
//! runtime handlers are tested/authoritative; these OUT_DIR copies are for
//! packaging convenience and a future change may switch to include!-ing a
//! shared cli module.

use std::io::Write;
use std::path::PathBuf;

use clap::{Command, ValueEnum};
use clap_complete::Shell;

const BIN_NAME: &str = "balanze-cli";

/// Minimal build-local mirror of the top-level command. Only the name and
/// `about` are load-bearing for packaging artifacts. Subcommand fidelity is
/// intentionally NOT mirrored here - see the module doc on drift.
fn build_command() -> Command {
    Command::new(BIN_NAME).about("Personal AI usage in one normalized view")
}

fn main() {
    // Only re-run when this script changes; the artifacts have no other
    // inputs and we must not trigger a rebuild storm.
    println!("cargo:rerun-if-changed=build.rs");

    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        // No OUT_DIR (should never happen under cargo); skip silently rather
        // than panic and break the build.
        println!("cargo:warning=balanze-cli build.rs: OUT_DIR unset, skipping artifact generation");
        return;
    };

    // Completions for every shell clap_complete knows about. A failure to
    // write any single artifact is downgraded to a warning so the build
    // still succeeds (the artifacts are packaging-only).
    for &shell in Shell::value_variants() {
        let mut cmd = build_command();
        if let Err(e) = clap_complete::generate_to(shell, &mut cmd, BIN_NAME, &out_dir) {
            println!("cargo:warning=balanze-cli: skipped {shell} completion: {e}");
        }
    }

    // Man page roff into OUT_DIR/balanze-cli.1.
    let man = clap_mangen::Man::new(build_command());
    let mut buf: Vec<u8> = Vec::new();
    if let Err(e) = man.render(&mut buf) {
        println!("cargo:warning=balanze-cli: man render failed: {e}");
        return;
    }
    let man_path = out_dir.join(format!("{BIN_NAME}.1"));
    if let Err(e) = std::fs::File::create(&man_path).and_then(|mut f| f.write_all(&buf)) {
        println!(
            "cargo:warning=balanze-cli: man write failed ({}): {e}",
            man_path.display()
        );
    }
}
