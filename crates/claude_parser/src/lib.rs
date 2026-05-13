//! Parse Claude Code session JSONL files into a normalized usage event stream.
//!
//! Claude Code writes one JSON object per line to `~/.claude/projects/<project>/<session>.jsonl`
//! (plus subagent files under `<session>/subagents/agent-*.jsonl`). Only lines with
//! `type == "assistant"` carry billing-relevant data; other line types (session
//! metadata, hooks, file-history snapshots, user messages, etc.) are filtered.
//!
//! Single-file parse contract: malformed JSON or unexpected shape at a position
//! we care about is reported as `ParseError::SchemaDrift { line, message }` so
//! the caller can map the failure into a `DegradedState` rather than aborting.

mod parser;
mod types;
mod walker;

pub use parser::{parse_line, parse_str};
pub use types::{AccountType, DataSource, ParseError, Provider, UsageEvent};
pub use walker::{candidate_claude_projects_dirs, find_claude_projects_dir, find_jsonl_files};
