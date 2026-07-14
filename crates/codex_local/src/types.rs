//! Public types: `CodexQuotaSnapshot` and `RateLimitWindow`.
//!
//! Single-value output shape per the design decision: the
//! Codex 4-quadrant matrix cell needs ONE number (the latest rate-limit
//! utilization), not a stream of events. See `SCHEMA-NOTES.md` for the
//! reasoning.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which rolling window a [`RateLimitWindow`] represents, classified by its
/// duration. Codex reports two lengths in practice: 300 minutes (5 hours)
/// and 10080 minutes (7 days / weekly). Which JSON slot (`primary`/`secondary`)
/// holds which VARIES by plan, CLI version, and over time: on "go" a single
/// weekly window sits in `primary`; on "plus"/"pro" `primary` WAS the 5-hour
/// window with `secondary` weekly, until 2026-07-12, when OpenAI temporarily
/// lifted the 5-hour limit and those plans began reporting the weekly window
/// alone in `primary` (`secondary: null`). Consumers MUST classify by duration,
/// never by slot, and MUST tolerate either window being absent - that is how a
/// window's removal or return arrives, with no code change on our side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    FiveHour,
    Weekly,
    Other,
}

/// Per-session token/context accounting from the `token_count` event's `info`
/// block. INTERNAL ONLY: `#[serde(skip)]` on the snapshot keeps these off the
/// IPC wire. Parsed and tested now; surfacing them in any UI is deferred
/// (see SCHEMA-NOTES.md "Token/context: internal only").
///
/// LIMITATION: Codex's cap is percentage-windows, not tokens, so token burn
/// does NOT predict quota exhaustion (unlike Claude, whose cap IS tokens). The
/// eventual actionable metric is context-window fill
/// (`last_input_tokens` / `context_window`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CodexTokenUsage {
    pub context_window: u64,
    pub last_input_tokens: u64,
    pub last_total_tokens: u64,
    pub session_total_tokens: u64,
    /// Tokens/min between the last two `token_count` events in the session.
    /// `None` with fewer than two events or a non-monotonic counter.
    pub recent_burn_tokens_per_min: Option<f64>,
}

/// Codex credits balance. INTERNAL ONLY (see [`CodexTokenUsage`]). For observed
/// data this is effectively always zero/absent; the real balance and per-model
/// (Spark) caps are backend-only and NOT obtainable from local files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CodexCredits {
    pub has_credits: bool,
    pub balance: Option<i64>,
}

impl RateLimitWindow {
    /// Classify this window by its duration. See [`WindowKind`].
    pub fn kind(&self) -> WindowKind {
        match self.window_duration_minutes {
            300 => WindowKind::FiveHour,
            10080 => WindowKind::Weekly,
            _ => WindowKind::Other,
        }
    }
}

impl CodexQuotaSnapshot {
    /// All present windows: `primary` always, `secondary` if any.
    pub fn windows(&self) -> impl Iterator<Item = &RateLimitWindow> {
        std::iter::once(&self.primary).chain(self.secondary.iter())
    }
    /// The 5-hour window, if present in either slot. `None` on plans that only
    /// expose a weekly window (e.g. "go").
    pub fn five_hour(&self) -> Option<&RateLimitWindow> {
        self.windows().find(|w| w.kind() == WindowKind::FiveHour)
    }
    /// The weekly (7-day) window, if present in either slot.
    pub fn weekly(&self) -> Option<&RateLimitWindow> {
        self.windows().find(|w| w.kind() == WindowKind::Weekly)
    }
    /// The highest-utilization window ("how close to a limit am I"). Always
    /// `Some` because `primary` is always present.
    pub fn worst_window(&self) -> Option<&RateLimitWindow> {
        self.windows()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    }
}

/// One Codex rate-limit window. Classify by duration
/// ([`RateLimitWindow::kind`]), never by which slot (`primary`/`secondary`)
/// it occupies - the slot-to-duration mapping varies by plan (see
/// [`WindowKind`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    /// Percentage of the window consumed. Range `0.0..100.0`; values
    /// equal to 100 (or marginally above due to server-side rounding)
    /// indicate the window is exhausted. Use [`CodexQuotaSnapshot::rate_limit_reached`]
    /// for the boolean "am I currently rate-limited" question, since
    /// the server may flag it separately from a strict 100%.
    pub used_percent: f64,
    /// Window length in minutes. Observed values: 300 (5 hours) and
    /// 10080 (7 days / weekly). Classify with [`RateLimitWindow::kind`]
    /// rather than assuming a slot maps to a fixed duration.
    pub window_duration_minutes: u64,
    /// Wall-clock instant when this window's counter resets to zero.
    /// Converted from Codex's unix-seconds field.
    pub resets_at: DateTime<Utc>,
}

/// A single snapshot of the user's Codex rate-limit state, extracted
/// from the most recent `token_count` event in the most recent session
/// file under `~/.codex/sessions/`.
///
/// This is the serialized public payload of the crate. The "Codex %" cell
/// of Balanze's 4-quadrant matrix reads [`CodexQuotaSnapshot::worst_window`]
/// (the highest-utilization window), not a fixed slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexQuotaSnapshot {
    /// Wall-clock instant when Codex CLI recorded this rate-limit state
    /// (top-level `timestamp` field of the source line).
    pub observed_at: DateTime<Utc>,
    /// Codex session UUID (from the session_meta line's `payload.id`).
    /// Useful for "this is the latest data from session X" debugging
    /// and for cross-referencing with Codex CLI's own logs.
    pub session_id: String,
    /// First rolling window slot. Always present. Its duration varies by
    /// plan (weekly on "go", 5-hour on "plus"/"pro"), so classify it by
    /// duration, not by being the `primary` slot. See [`WindowKind`].
    pub primary: RateLimitWindow,
    /// Optional second rolling window slot. `None` on plans that expose a
    /// single window (e.g. "go"); on "plus"/"pro" it holds the weekly
    /// window. Classify by duration, not by slot.
    pub secondary: Option<RateLimitWindow>,
    /// Plan-type string from Codex CLI. Observed: "go". Other values
    /// in the wild may include "pro", "team", "enterprise" - display
    /// only, don't gate logic on this string.
    pub plan_type: String,
    /// True when Codex has actively rate-limited the user (server
    /// surfaces a non-null `rate_limit_reached_type`). Distinct from
    /// `primary.used_percent >= 100.0` because the server may flag
    /// rate-limiting before or after the strict 100% threshold.
    pub rate_limit_reached: bool,
    /// INTERNAL, not serialized (`#[serde(skip)]`). Token/context accounting;
    /// deferred from UI. See [`CodexTokenUsage`].
    #[serde(skip)]
    pub tokens: Option<CodexTokenUsage>,
    /// INTERNAL, not serialized. Credits balance; deferred from UI.
    #[serde(skip)]
    pub credits: Option<CodexCredits>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn win(pct: f64, mins: u64) -> RateLimitWindow {
        RateLimitWindow {
            used_percent: pct,
            window_duration_minutes: mins,
            resets_at: Utc.timestamp_opt(2000, 0).unwrap(),
        }
    }
    fn snap(primary: RateLimitWindow, secondary: Option<RateLimitWindow>) -> CodexQuotaSnapshot {
        CodexQuotaSnapshot {
            observed_at: Utc.timestamp_opt(1000, 0).unwrap(),
            session_id: "s".into(),
            primary,
            secondary,
            plan_type: "pro".into(),
            rate_limit_reached: false,
            tokens: None,
            credits: None,
        }
    }

    #[test]
    fn classifies_and_selects_windows_by_duration_not_slot() {
        // plus/pro order: primary=5h, secondary=weekly.
        let s = snap(win(1.0, 300), Some(win(2.0, 10080)));
        assert_eq!(s.five_hour().unwrap().used_percent, 1.0);
        assert_eq!(s.weekly().unwrap().used_percent, 2.0);
        assert_eq!(s.worst_window().unwrap().used_percent, 2.0);
        // go order: single weekly window in primary.
        let g = snap(win(3.0, 10080), None);
        assert!(g.five_hour().is_none());
        assert_eq!(g.weekly().unwrap().used_percent, 3.0);
        assert_eq!(g.worst_window().unwrap().used_percent, 3.0);
    }

    #[test]
    fn internal_fields_are_not_serialized() {
        let mut s = snap(win(1.0, 300), Some(win(2.0, 10080)));
        s.tokens = Some(CodexTokenUsage {
            context_window: 258400,
            session_total_tokens: 999,
            ..Default::default()
        });
        s.credits = Some(CodexCredits {
            has_credits: false,
            balance: Some(0),
        });
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("tokens"), "{json}");
        assert!(!json.contains("credits"), "{json}");
        assert!(!json.contains("context_window"), "{json}");
        // round-trips back to None (skipped fields default on deserialize).
        let back: CodexQuotaSnapshot = serde_json::from_str(&json).unwrap();
        assert!(back.tokens.is_none() && back.credits.is_none());
    }
}
