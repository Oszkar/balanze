//! Message types for the coordinator actor's mpsc channel.

use anthropic_oauth::ClaudeOAuthSnapshot;
use openai_client::OpenAiCosts;
use settings::Settings;
use tokio::sync::oneshot;

use crate::snapshot::{JsonlSnapshot, Snapshot};

/// Which source produced an update or failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    ClaudeOAuth,
    ClaudeJsonl,
    OpenAiCosts,
}

/// Successful data payload from one source. The variant identifies the source;
/// the inner type is whatever that source produces.
#[derive(Debug, Clone)]
pub enum SourcePartial {
    ClaudeOAuth(ClaudeOAuthSnapshot),
    ClaudeJsonl(JsonlSnapshot),
    OpenAiCosts(OpenAiCosts),
}

impl SourcePartial {
    pub fn source(&self) -> Source {
        match self {
            SourcePartial::ClaudeOAuth(_) => Source::ClaudeOAuth,
            SourcePartial::ClaudeJsonl(_) => Source::ClaudeJsonl,
            SourcePartial::OpenAiCosts(_) => Source::OpenAiCosts,
        }
    }
}

/// What a poller sends to the coordinator after a fetch attempt.
///
/// `result: Ok(partial)` → merge_partial into the snapshot, clear the source's
/// error slot, notify `Sink::on_snapshot`.
///
/// `result: Err(message)` → keep any existing data, set the source's error
/// slot, notify `Sink::on_degraded`. Existing data stays visible (UI renders
/// stale-with-indicator rather than blank).
#[derive(Debug, Clone)]
pub struct SourceUpdate {
    pub source: Source,
    pub result: Result<SourcePartial, String>,
}

/// The coordinator's input language. One variant per architectural input
/// path; see AGENTS.md §4 #7 for the data-flow diagram.
#[derive(Debug)]
pub enum StateMsg {
    /// A poller has finished a fetch — apply the result.
    Update(SourceUpdate),
    /// Tauri command (or test): read the current Snapshot via the oneshot reply.
    Query(oneshot::Sender<Snapshot>),
    /// 30s tray ticker or manual refresh: re-notify the Sink with current state
    /// so it can repaint. The coordinator itself does NOT fetch — refreshes are
    /// re-paints, not re-fetches. Pollers run on their own cadence.
    Refresh,
    /// Settings file changed. Scaffold stores the value; future pollers will
    /// subscribe to a settings-change broadcast and reconfigure themselves.
    SettingsChanged(Settings),
}
