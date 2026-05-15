//! Message types for the coordinator actor's mpsc channel.

use anthropic_oauth::ClaudeOAuthSnapshot;
use claude_cost::Cost;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use settings::Settings;
use tokio::sync::oneshot;

use crate::snapshot::{JsonlSnapshot, Snapshot};

/// Which source produced an update or failure.
///
/// The five sources map 1-to-1 onto the cells / inputs of Balanze's
/// 4-quadrant matrix:
///
/// ```text
///           | Subscription / Quota (%)        | API / Pay-as-you-go ($)
/// ----------|---------------------------------|---------------------------------
/// Anthropic | ClaudeOAuth                     | AnthropicApiCost (from ClaudeJsonl)
/// OpenAI    | CodexQuota                      | OpenAiCosts
/// ```
///
/// `ClaudeJsonl` is the raw JSONL input that powers both Anthropic cells
/// (window math for the quota display, token counts for AnthropicApiCost).
/// It's a separate Source because its update cadence and failure modes
/// differ from the API-rate cost derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// `anthropic_oauth::fetch_usage` — the 5h / 7d / per-model cadence bars.
    ClaudeOAuth,
    /// `claude_parser` + `window::summarize_window` — local JSONL activity.
    ClaudeJsonl,
    /// `claude_cost::compute_cost` — Anthropic API-rate cost estimate.
    AnthropicApiCost,
    /// `codex_local::read_codex_quota` — OpenAI Codex CLI rate-limit snapshot.
    CodexQuota,
    /// `openai_client::costs_this_month` — month-to-date OpenAI spend.
    OpenAiCosts,
}

/// Successful data payload from one source. The variant identifies the source;
/// the inner type is whatever that source produces.
#[derive(Debug, Clone)]
pub enum SourcePartial {
    ClaudeOAuth(ClaudeOAuthSnapshot),
    ClaudeJsonl(JsonlSnapshot),
    AnthropicApiCost(Cost),
    CodexQuota(CodexQuotaSnapshot),
    OpenAiCosts(OpenAiCosts),
}

impl SourcePartial {
    /// Map a partial back to its [`Source`] tag. Used by the coordinator to
    /// cross-check `SourceUpdate.source` against the payload variant.
    pub fn source(&self) -> Source {
        match self {
            SourcePartial::ClaudeOAuth(_) => Source::ClaudeOAuth,
            SourcePartial::ClaudeJsonl(_) => Source::ClaudeJsonl,
            SourcePartial::AnthropicApiCost(_) => Source::AnthropicApiCost,
            SourcePartial::CodexQuota(_) => Source::CodexQuota,
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
