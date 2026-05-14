use std::path::PathBuf;

use thiserror::Error;

/// Errors surfaced by `codex_local`.
///
/// Codex CLI is a fast-moving upstream; treating schema variation as a
/// typed error (rather than a panic or a silent skip) lets callers
/// degrade gracefully via `state_coordinator`'s `DegradedState` machinery
/// per AGENTS.md §3.2.
#[derive(Debug, Error)]
pub enum ParseError {
    /// The expected `~/.codex/sessions/` directory (or a path passed in
    /// explicitly) doesn't exist. Caller maps this to
    /// `DegradedState::CodexDirMissing` and shows the Codex row as
    /// "not configured" rather than an error.
    #[error("file or directory not found: {0}")]
    FileMissing(PathBuf),

    /// IO error while walking or reading a session file. Distinct from
    /// `FileMissing` so the caller can tell "Codex isn't installed"
    /// (graceful) apart from "filesystem is broken" (loud).
    #[error("io error reading {path:?}: {source}")]
    IoError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A JSONL line failed to parse, or a `token_count` event was
    /// present but its `rate_limits.primary` block had unexpected shape.
    /// `line` is 1-indexed (matches the file when grepping). The parser
    /// continues past schema drift on individual lines — the latest
    /// well-formed `token_count` event still gets extracted — but
    /// callers can use the count of drift events for telemetry if they
    /// care.
    #[error("schema drift on line {line} of {path:?}: {message}")]
    SchemaDrift {
        path: PathBuf,
        line: usize,
        message: String,
    },
}
