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
    Green,
    Yellow,
    Orange,
    Red,
    Warn,
}

impl ColorBucket {
    pub(crate) fn from_util(util_percent: f32) -> Self {
        if util_percent >= 90.0 {
            ColorBucket::Red
        } else if util_percent >= 75.0 {
            ColorBucket::Orange
        } else if util_percent >= 50.0 {
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
        let bucket = if degraded {
            ColorBucket::Warn
        } else {
            ColorBucket::from_util(worst_utilization(s))
        };
        (bucket, tray_title(s))
    }
}

impl Sink for TauriSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        if let Err(e) = self.app.emit("usage_updated", snapshot) {
            tracing::warn!("tauri_sink: emit usage_updated failed: {e}");
        }
        let target = self.paint_target(snapshot, false);
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
    fn bucket_thresholds() {
        assert_eq!(ColorBucket::from_util(0.0), ColorBucket::Green);
        assert_eq!(ColorBucket::from_util(49.9), ColorBucket::Green);
        assert_eq!(ColorBucket::from_util(50.0), ColorBucket::Yellow);
        assert_eq!(ColorBucket::from_util(74.9), ColorBucket::Yellow);
        assert_eq!(ColorBucket::from_util(75.0), ColorBucket::Orange);
        assert_eq!(ColorBucket::from_util(90.0), ColorBucket::Red);
        assert_eq!(ColorBucket::from_util(150.0), ColorBucket::Red);
    }

    #[test]
    fn worst_util_picks_max_across_sources() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        assert_eq!(worst_utilization(&s), 0.0);
        s.claude_oauth = Some(anthropic_oauth::ClaudeOAuthSnapshot {
            cadences: vec![anthropic_oauth::CadenceBar {
                key: "five_hour".into(),
                display_label: "5h".into(),
                utilization_percent: 62.0,
                resets_at: Utc::now(),
            }],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: Utc::now(),
        });
        assert_eq!(worst_utilization(&s), 62.0);
    }
}
