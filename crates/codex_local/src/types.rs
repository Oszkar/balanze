//! Public types: `CodexQuotaSnapshot` and `RateLimitWindow`.
//!
//! Single-value output shape per the post-spike design decision: the
//! Codex 4-quadrant matrix cell needs ONE number (the latest rate-limit
//! utilization), not a stream of events. See `SCHEMA-NOTES.md` for the
//! reasoning.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One Codex rate-limit window — `primary` is always present in
/// observed data (7-day rolling); `secondary` may carry a 5-hour
/// sub-window on higher-tier plans (not observed on the spike's "go"
/// plan but documented in the Codex CLI schema as `Option`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    /// Percentage of the window consumed. Range `0.0..100.0`; values
    /// equal to 100 (or marginally above due to server-side rounding)
    /// indicate the window is exhausted. Use [`CodexQuotaSnapshot::rate_limit_reached`]
    /// for the boolean "am I currently rate-limited" question, since
    /// the server may flag it separately from a strict 100%.
    pub used_percent: f64,
    /// Window length in minutes. Observed values: 10080 (7 days)
    /// for the primary window. A future secondary window may show
    /// 300 (5 hours).
    pub window_duration_minutes: u64,
    /// Wall-clock instant when this window's counter resets to zero.
    /// Converted from Codex's unix-seconds field.
    pub resets_at: DateTime<Utc>,
}

/// A single snapshot of the user's Codex rate-limit state, extracted
/// from the most recent `token_count` event in the most recent session
/// file under `~/.codex/sessions/`.
///
/// This is the entire public payload of the crate. The "Codex %" cell
/// of Balanze's 4-quadrant matrix is `primary.used_percent` from here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexQuotaSnapshot {
    /// Wall-clock instant when Codex CLI recorded this rate-limit state
    /// (top-level `timestamp` field of the source line).
    pub observed_at: DateTime<Utc>,
    /// Codex session UUID (from the session_meta line's `payload.id`).
    /// Useful for "this is the latest data from session X" debugging
    /// and for cross-referencing with Codex CLI's own logs.
    pub session_id: String,
    /// Primary rolling window. Always present in observed data.
    pub primary: RateLimitWindow,
    /// Optional secondary (shorter) window. None for "go"/free-tier
    /// plans in observed data; populated for higher-tier plans that
    /// have a 5-hour sub-limit.
    pub secondary: Option<RateLimitWindow>,
    /// Plan-type string from Codex CLI. Observed: "go". Other values
    /// in the wild may include "pro", "team", "enterprise" — display
    /// only, don't gate logic on this string.
    pub plan_type: String,
    /// True when Codex has actively rate-limited the user (server
    /// surfaces a non-null `rate_limit_reached_type`). Distinct from
    /// `primary.used_percent >= 100.0` because the server may flag
    /// rate-limiting before or after the strict 100% threshold.
    pub rate_limit_reached: bool,
}
