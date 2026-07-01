//! Parses the Claude Code `statusLine` stdin payload and owns the
//! `statusLine` stanza in Claude Code's `settings.json`.
//!
//! Sits in the schema-owning data-source tier alongside `claude_parser`
//! (§4 #1) and `codex_local` (§4 #11): it is the ONLY code that knows the
//! statusLine wire format, and — mirroring `anthropic_oauth` for
//! `.credentials.json` — also the only code that reads/writes the
//! `statusLine` key in Claude's `settings.json`.
//!
//! `rate_limits` is Pro/Max-only and only present after the first API
//! response in a session; absent is `None`, never an error. The payload
//! schema evolves (e.g. `context_window.*` at v2.1.132) so unknown/missing
//! fields are tolerated. The watcher (not this crate) wires the parsed snapshot
//! into the live Snapshot/coordinator.

pub mod errors;
pub mod file_io;
pub mod parse;
pub mod payload;
pub mod types;
pub mod wiring;

pub use errors::StatuslineError;
pub use file_io::{FileIoError, atomic_write_snapshot, read_snapshot};
pub use parse::parse;
pub use payload::{SCHEMA_VERSION, StatuslineFilePayload};
pub use types::{RateLimits, RateWindow, StatuslineSnapshot};
pub use wiring::{
    STATUSLINE_INVOCATION, WireStatus, default_settings_path, locate_settings_path,
    read_wire_status, restore_statusline, unwire_statusline, wire_statusline,
};
