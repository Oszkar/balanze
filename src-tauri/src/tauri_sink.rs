//! Production sink for the `state_coordinator` actor (AGENTS.md §4 #7 — the
//! only crate that may call Tauri tray APIs). Emits `usage_updated` /
//! `degraded_state` to the Svelte UI and repaints the gauge tray icon,
//! deduped by `(ColorBucket, title)` per AGENTS.md §3.1.

use serde::Serialize;
use state_coordinator::{Sink, Snapshot, Source};
use tauri::{AppHandle, Emitter};

use crate::tray_icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorBucket {
    /// No quota signal from any source yet (cold start, or no provider
    /// configured). Painted grey by `select_bucket` so an empty state never
    /// reads as a healthy green.
    Neutral,
    Green,
    Yellow,
    Orange,
    Red,
    Warn,
}

/// Canonical quota-color thresholds (percent utilization). The popover mirrors
/// the WARN and BAD boundaries in `src/lib/presentation/quota.ts` (`quotaTone`);
/// that surface uses a coarser 3-tone palette and folds this ORANGE band into
/// "warn". Keep the shared boundary values (50, 90) in lockstep across the two
/// files.
const QUOTA_WARN_PCT: f32 = 50.0;
const QUOTA_ORANGE_PCT: f32 = 75.0;
const QUOTA_BAD_PCT: f32 = 90.0;

impl ColorBucket {
    pub(crate) fn from_util(util_percent: f32) -> Self {
        if util_percent >= QUOTA_BAD_PCT {
            ColorBucket::Red
        } else if util_percent >= QUOTA_ORANGE_PCT {
            ColorBucket::Orange
        } else if util_percent >= QUOTA_WARN_PCT {
            ColorBucket::Yellow
        } else {
            ColorBucket::Green
        }
    }
}

#[derive(Serialize, Clone)]
struct DegradedPayload {
    source: String,
    error: String,
}

fn source_key(source: Source) -> &'static str {
    match source {
        Source::ClaudeOAuth => "claude_oauth",
        Source::ClaudeJsonl => "claude_jsonl",
        Source::AnthropicApiCost => "anthropic_api_cost",
        Source::CodexQuota => "codex_quota",
        Source::OpenAiCosts => "openai_costs",
        Source::ClaudeStatusline => "claude_statusline",
    }
}

pub(crate) fn worst_utilization(s: &Snapshot) -> f32 {
    let mut worst = 0.0_f32;
    if let Some(o) = &s.claude_oauth {
        for c in &o.cadences {
            worst = worst.max(c.utilization_percent);
        }
    }
    if let Some(sl) = &s.claude_statusline {
        if let Some(rl) = &sl.payload.rate_limits {
            if let Some(w) = &rl.five_hour {
                worst = worst.max(w.used_percent);
            }
            if let Some(w) = &rl.seven_day {
                worst = worst.max(w.used_percent);
            }
        }
    }
    if let Some(c) = &s.codex_quota {
        worst = worst.max(c.primary.used_percent as f32);
    }
    worst
}

/// True when any source carries an actual quota signal - the same leaves
/// `worst_utilization` reads. This distinguishes a real 0% utilization (paint
/// the heat color) from a cold-start / not-configured snapshot with no signal
/// at all (paint Neutral). Without it, an empty snapshot's 0.0 worst-util would
/// map to Green and read as "all good" before any data has arrived.
fn has_quota_data(s: &Snapshot) -> bool {
    let oauth = s
        .claude_oauth
        .as_ref()
        .is_some_and(|o| !o.cadences.is_empty());
    let statusline = s
        .claude_statusline
        .as_ref()
        .and_then(|sl| sl.payload.rate_limits.as_ref())
        .is_some_and(|rl| rl.five_hour.is_some() || rl.seven_day.is_some());
    oauth || statusline || s.codex_quota.is_some()
}

/// Pick the tray color for a snapshot. A degraded source forces the warning
/// bucket; an empty snapshot (no quota signal yet) is Neutral; otherwise the
/// worst-case utilization across sources maps to a heat color.
fn select_bucket(s: &Snapshot, degraded: bool) -> ColorBucket {
    if degraded {
        ColorBucket::Warn
    } else if !has_quota_data(s) {
        ColorBucket::Neutral
    } else {
        ColorBucket::from_util(worst_utilization(s))
    }
}

fn tray_title(s: &Snapshot) -> String {
    let c = s
        .claude_oauth
        .as_ref()
        .and_then(|o| o.cadences.iter().find(|c| c.key == "five_hour"))
        .map(|c| format!("C {:.0}%", c.utilization_percent));
    let o = s
        .codex_quota
        .as_ref()
        .map(|q| format!("O {:.0}%", q.primary.used_percent));
    [c, o].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

pub(crate) struct TauriSink {
    app: AppHandle,
    last_painted: Option<(ColorBucket, String)>,
}

impl TauriSink {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
            app,
            last_painted: None,
        }
    }

    fn paint_target(&self, s: &Snapshot, degraded: bool) -> (ColorBucket, String) {
        (select_bucket(s, degraded), tray_title(s))
    }
}

/// True if any source's error slot is set. Used to keep the tray on the
/// warning bucket while ANY source is degraded — a later success from a
/// different source must not clear the warning while another source is still
/// failing (the bug: `on_snapshot` previously hard-coded `degraded = false`).
fn any_source_degraded(s: &Snapshot) -> bool {
    s.claude_oauth_error.is_some()
        || s.claude_jsonl_error.is_some()
        || s.anthropic_api_cost_error.is_some()
        || s.codex_quota_error.is_some()
        || s.openai_error.is_some()
        || s.claude_statusline_error.is_some()
}

impl Sink for TauriSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        if let Err(e) = self.app.emit("usage_updated", snapshot) {
            tracing::warn!("tauri_sink: emit usage_updated failed: {e}");
        }
        let target = self.paint_target(snapshot, any_source_degraded(snapshot));
        if self.last_painted.as_ref() != Some(&target) {
            tray_icon::paint(&self.app, target.0, &target.1);
            self.last_painted = Some(target);
        }
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        let payload = DegradedPayload {
            source: source_key(source).to_string(),
            error: error.to_string(),
        };
        if let Err(e) = self.app.emit("degraded_state", payload) {
            tracing::warn!("tauri_sink: emit degraded_state failed: {e}");
        }
        let title = self
            .last_painted
            .as_ref()
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        let target = (ColorBucket::Warn, title);
        if self.last_painted.as_ref() != Some(&target) {
            tray_icon::paint(&self.app, target.0, &target.1);
            self.last_painted = Some(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_source_error_keeps_warning() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        assert!(!any_source_degraded(&s));
        s.openai_error = Some("HTTP 500".into());
        assert!(any_source_degraded(&s));
    }

    #[test]
    fn bucket_thresholds() {
        assert_eq!(ColorBucket::from_util(0.0), ColorBucket::Green);
        assert_eq!(ColorBucket::from_util(49.9), ColorBucket::Green);
        assert_eq!(ColorBucket::from_util(50.0), ColorBucket::Yellow);
        assert_eq!(ColorBucket::from_util(74.9), ColorBucket::Yellow);
        assert_eq!(ColorBucket::from_util(75.0), ColorBucket::Orange);
        assert_eq!(ColorBucket::from_util(90.0), ColorBucket::Red);
        assert_eq!(ColorBucket::from_util(150.0), ColorBucket::Red);
    }

    /// Build a Claude OAuth snapshot carrying a single 5-hour cadence at the
    /// given utilization, shared by the worst-util and bucket-selection tests.
    fn oauth_with_util(util: f32) -> anthropic_oauth::ClaudeOAuthSnapshot {
        use chrono::Utc;
        anthropic_oauth::ClaudeOAuthSnapshot {
            cadences: vec![anthropic_oauth::CadenceBar {
                key: "five_hour".into(),
                display_label: "5h".into(),
                utilization_percent: util,
                resets_at: Utc::now(),
            }],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn worst_util_picks_max_across_sources() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        assert_eq!(worst_utilization(&s), 0.0);
        s.claude_oauth = Some(oauth_with_util(62.0));
        assert_eq!(worst_utilization(&s), 62.0);
    }

    #[test]
    fn empty_snapshot_has_no_quota_data() {
        use chrono::Utc;
        assert!(!has_quota_data(&Snapshot::empty(Utc::now())));
    }

    #[test]
    fn oauth_cadence_counts_as_quota_data() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_with_util(0.0));
        assert!(has_quota_data(&s));
    }

    #[test]
    fn empty_snapshot_paints_neutral_not_green() {
        use chrono::Utc;
        // The fix: a cold-start snapshot must read as Neutral, never the healthy
        // Green that worst_utilization's 0.0 would otherwise produce.
        assert_eq!(
            select_bucket(&Snapshot::empty(Utc::now()), false),
            ColorBucket::Neutral
        );
    }

    #[test]
    fn populated_zero_util_is_green_not_neutral() {
        use chrono::Utc;
        // Real data at 0% is healthy Green - distinct from "no data" Neutral.
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_with_util(0.0));
        assert_eq!(select_bucket(&s, false), ColorBucket::Green);
    }

    #[test]
    fn degraded_overrides_neutral() {
        use chrono::Utc;
        // A failing source paints the warning bucket even with no quota data.
        assert_eq!(
            select_bucket(&Snapshot::empty(Utc::now()), true),
            ColorBucket::Warn
        );
    }
}
