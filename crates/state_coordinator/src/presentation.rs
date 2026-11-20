//! Snapshot adapters for shared quota presentation decisions.

use crate::{AnthropicQuotaSource, Snapshot};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicPresentationSource {
    Statusline,
    OAuth,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnthropicPresentationWindow<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub percent: f32,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnthropicPresentation<'a> {
    pub source: AnthropicPresentationSource,
    pub stale: bool,
    pub windows: window::AnthropicWindows<AnthropicPresentationWindow<'a>>,
}

impl Snapshot {
    /// Selected source and compact window slots, anchored to snapshot time.
    pub fn anthropic_presentation(&self) -> Option<AnthropicPresentation<'_>> {
        let (source, stale, windows) = match self.anthropic_quota_source()? {
            AnthropicQuotaSource::Statusline { rate_limits, stale } => {
                let windows = rate_limits.windows.iter().map(|w| {
                    let view = AnthropicPresentationWindow {
                        key: &w.key,
                        label: &w.label,
                        percent: w.used_percent,
                        resets_at: w.resets_at,
                    };
                    (view, view.key, view.percent)
                });
                (
                    AnthropicPresentationSource::Statusline,
                    stale,
                    window::select_anthropic_windows(windows),
                )
            }
            AnthropicQuotaSource::OAuth { snapshot, stale } => {
                let windows = snapshot.cadences.iter().map(|w| {
                    let view = AnthropicPresentationWindow {
                        key: &w.key,
                        label: &w.display_label,
                        percent: w.utilization_percent,
                        resets_at: w.resets_at,
                    };
                    (view, view.key, view.percent)
                });
                (
                    AnthropicPresentationSource::OAuth,
                    stale,
                    window::select_anthropic_windows(windows),
                )
            }
        };
        Some(AnthropicPresentation {
            source,
            stale,
            windows,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot};
    use chrono::Duration;
    use claude_statusline::{RateLimits, RateWindow, StatuslineFilePayload, StatuslineSnapshot};

    #[test]
    fn selected_source_carries_its_windows_and_staleness() {
        let now = Utc::now();
        let mut s = Snapshot::empty(now);
        s.claude_oauth = Some(ClaudeOAuthSnapshot {
            cadences: vec![CadenceBar {
                key: "five_hour".into(),
                display_label: "OAuth".into(),
                utilization_percent: 10.0,
                resets_at: now + Duration::hours(1),
            }],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: now,
        });
        s.claude_statusline = Some(StatuslineFilePayload::new(
            StatuslineSnapshot {
                rate_limits: Some(RateLimits {
                    windows: vec![RateWindow {
                        key: "five_hour".into(),
                        label: "live".into(),
                        used_percent: 80.0,
                        resets_at: now + Duration::hours(1),
                    }],
                }),
                session_cost_micro_usd: None,
                claude_code_version: None,
                model_display_name: None,
                context_used_percent: None,
            },
            now,
        ));
        let live = s.anthropic_presentation().unwrap();
        assert_eq!(live.source, AnthropicPresentationSource::Statusline);
        assert!(!live.stale);
        assert_eq!(live.windows.five_hour.unwrap().percent, 80.0);
        s.claude_statusline_error = Some("failed".into());
        let fallback = s.anthropic_presentation().unwrap();
        assert_eq!(fallback.source, AnthropicPresentationSource::OAuth);
        assert!(!fallback.stale);
        assert_eq!(fallback.windows.five_hour.unwrap().percent, 10.0);
        s.claude_oauth_error = Some("failed".into());
        assert!(s.anthropic_presentation().unwrap().stale);
    }
}
