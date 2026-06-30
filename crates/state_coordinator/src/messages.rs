//! Message types for the coordinator actor's mpsc channel.

use std::sync::Arc;

use anthropic_oauth::ClaudeOAuthSnapshot;
use claude_parser::UsageEvent;
use claude_statusline::StatuslineFilePayload;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use settings::Settings;
use tokio::sync::oneshot;

use crate::snapshot::Snapshot;

/// Which source produced an update or failure.
///
/// The six sources map onto the cells / inputs of Balanze's display matrix:
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
///
/// `ClaudeStatusline` carries the parsed statusLine payload written by
/// `balanze-cli statusline` and read by the watcher.
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
    /// `claude_statusline` — parsed statusLine payload from the statusline snapshot file.
    ClaudeStatusline,
}

/// Raw input for the JSONL cell: the deduped event slice + the count of files
/// scanned. The coordinator (and the one-shot `compose` path) derive BOTH the
/// window summary and the API-rate cost from these events via
/// [`crate::summarize_jsonl`], anchoring the window to the OAuth 5-hour reset.
///
/// Carrying raw events (rather than a pre-summarized `JsonlSnapshot`) is what
/// lets the coordinator re-anchor the window when a later OAuth update arrives
/// with the authoritative reset — the producer (CLI / watcher) no longer owns
/// the window math, so the two paths can't diverge (AGENTS.md §4 #8). `Arc` so
/// caching + re-anchoring never clones the vector.
#[derive(Debug, Clone)]
pub struct ClaudeJsonlInput {
    pub events: Arc<Vec<UsageEvent>>,
    pub files_scanned: usize,
}

/// Successful data payload from one source. The variant identifies the source;
/// the inner type is whatever that source produces.
///
/// Note there is no `AnthropicApiCost` variant: the API-rate cost is *derived*
/// from `ClaudeJsonl`'s events inside the coordinator (via `summarize_jsonl`),
/// never sent as its own partial. `Source::AnthropicApiCost` still exists as the
/// error-slot tag for that derived cell.
#[derive(Debug, Clone)]
pub enum SourcePartial {
    ClaudeOAuth(ClaudeOAuthSnapshot),
    ClaudeJsonl(ClaudeJsonlInput),
    CodexQuota(CodexQuotaSnapshot),
    OpenAiCosts(OpenAiCosts),
    ClaudeStatusline(StatuslineFilePayload),
}

impl SourcePartial {
    /// Map a partial back to its [`Source`] tag. Used by the coordinator to
    /// cross-check `SourceUpdate.source` against the payload variant.
    pub fn source(&self) -> Source {
        match self {
            SourcePartial::ClaudeOAuth(_) => Source::ClaudeOAuth,
            SourcePartial::ClaudeJsonl(_) => Source::ClaudeJsonl,
            SourcePartial::CodexQuota(_) => Source::CodexQuota,
            SourcePartial::OpenAiCosts(_) => Source::OpenAiCosts,
            SourcePartial::ClaudeStatusline(_) => Source::ClaudeStatusline,
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
    /// Popover open or manual refresh: re-notify the Sink with current state
    /// so it can repaint. The coordinator itself does NOT fetch — refreshes are
    /// re-paints, not re-fetches. Pollers run on their own cadence.
    Refresh,
    /// Settings file changed. Scaffold stores the value; future pollers will
    /// subscribe to a settings-change broadcast and reconfigure themselves.
    /// Boxed: `Settings` carries the full statusline config (~450 bytes), which
    /// would otherwise dominate this enum's size (clippy `large_enum_variant`).
    SettingsChanged(Box<Settings>),
    /// A source reports it is intentionally unavailable / not configured - e.g.
    /// Claude Code isn't installed, so the OAuth poller has no credential to
    /// read. A NEUTRAL state, distinct from `Update(Err(..))`: the cell reads
    /// "not configured" rather than degraded, and the tray stays on the neutral
    /// bucket (no `on_degraded`, no red). Only `ClaudeOAuth` carries a slot for
    /// it today; other sources are logged and ignored.
    SourceUnavailable { source: Source, reason: String },
}
