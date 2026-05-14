use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Which AI provider produced this event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    Claude,
    OpenAi,
}

/// How the event was billed — Anthropic subscription (Claude Max etc.) or
/// metered API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccountType {
    Subscription,
    Api,
}

/// Where this event's data came from. Surfaced in `--json` so consumers can
/// distinguish authoritative sources from inferred / scraped ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataSource {
    /// `~/.claude/projects/**/*.jsonl` — local Claude Code session files.
    Jsonl,
    /// OpenAI Admin Costs API (`/v1/organization/costs`).
    OpenAiBilling,
    /// Anthropic Console (cookie-based scrape, planned for v0.2).
    AnthropicConsole,
    /// Derived locally (e.g., burn-rate extrapolations); not authoritative.
    Inferred,
}

/// One billing-relevant assistant turn. Produced by `parse_str` and consumed
/// downstream by `dedup_events` (via `(message_id, request_id)`),
/// `window::summarize_window`, and eventually the predictor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEvent {
    /// Wall-clock timestamp Claude Code recorded for this turn (top-level
    /// `timestamp` field).
    pub ts: DateTime<Utc>,
    pub provider: Provider,
    pub account_type: AccountType,
    /// Anthropic model name (e.g. `claude-sonnet-4-6`). Empty string when the
    /// JSONL omits it.
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    /// Per-event cost in micro-USD (1e-6 USD). `None` for subscription events
    /// (Claude Code JSONL never carries cost; cost is derived for API events
    /// from the provider's billing endpoint, not from this stream).
    pub cost_micro_usd: Option<i64>,
    pub source: DataSource,
    /// `message.id` from the JSONL line ("msg_…"). First half of the dedup
    /// key. `None` for synthetic events or future schema drift.
    pub message_id: Option<String>,
    /// Top-level `requestId` from the JSONL line ("req_…"). Second half of
    /// the dedup key. `None` if absent.
    pub request_id: Option<String>,
}

impl UsageEvent {
    /// All tokens billed by Anthropic for this turn — input + output + both
    /// cache categories. This is the unit the Claude subscription 5-hour cap
    /// is denominated in; cache reads count even though they're cheaper at
    /// the API-billing layer.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens)
    }
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("file or directory not found: {0}")]
    FileMissing(std::path::PathBuf),

    #[error("permission denied reading {0}")]
    PermissionDenied(std::path::PathBuf),

    #[error("io error reading {path:?}: {source}")]
    IoError {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("schema drift on line {line}: {message}")]
    SchemaDrift { line: usize, message: String },
}
