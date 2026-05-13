use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    Claude,
    OpenAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccountType {
    Subscription,
    Api,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataSource {
    Jsonl,
    OpenAiBilling,
    AnthropicConsole,
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEvent {
    pub ts: DateTime<Utc>,
    pub provider: Provider,
    pub account_type: AccountType,
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
