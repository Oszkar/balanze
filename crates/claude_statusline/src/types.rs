use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One server-authoritative subscription window from the statusLine feed.
/// `anthropic_oauth::CadenceBar`'s analogous fields are `key`/`display_label`/
/// `utilization_percent`; `RateWindow` uses the shorter `used_percent` and
/// `resets_at: DateTime<Utc>`. The watcher aligns the two sources (a small
/// field-name mapping step).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    /// Raw wire key from the statusLine `rate_limits` object (e.g.
    /// `"five_hour"`, `"seven_day"`, or any future key Claude Code adds).
    pub key: String,
    /// Human-friendly display label, synthesized at parse time. Known keys
    /// map to curated strings (`"5-hour"`, `"7-day"`); unknown keys titlecase
    /// the raw key so a future addition still renders sensibly.
    pub label: String,
    pub used_percent: f32,
    pub resets_at: DateTime<Utc>,
}

/// All rate-limit windows from one statusLine payload. Generic over however
/// many named windows Claude Code reports - not just `five_hour`/`seven_day` -
/// mirroring `anthropic_oauth::ClaudeOAuthSnapshot`'s `cadences: Vec<CadenceBar>`
/// plus named-accessor shape for the same problem (an arbitrary, growing set
/// of named usage windows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    pub windows: Vec<RateWindow>,
}

impl RateLimits {
    /// The 5-hour session window, if present.
    pub fn five_hour(&self) -> Option<&RateWindow> {
        self.windows.iter().find(|w| w.key == "five_hour")
    }

    /// The 7-day "all models" window, if present.
    pub fn seven_day(&self) -> Option<&RateWindow> {
        self.windows.iter().find(|w| w.key == "seven_day")
    }
}

/// Parsed statusLine payload. `None` fields = "not present in this payload"
/// (e.g. `rate_limits` is Pro/Max-only and only after the first API
/// response). `session_cost_micro_usd` is a Claude-side SESSION ESTIMATE
/// (i64 micro-USD, AGENTS.md §2.1) - a distinct cost tier, never conflated
/// with the JSONL list-price estimate or the real `extra_usage` overage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatuslineSnapshot {
    pub rate_limits: Option<RateLimits>,
    pub session_cost_micro_usd: Option<i64>,
    pub claude_code_version: Option<String>,
    /// Human model name from `model.display_name` (e.g. "Opus 4.7 (1M context)").
    pub model_display_name: Option<String>,
    /// Context-window utilization percent from `context_window.used_percentage`.
    pub context_used_percent: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn five_hour_returns_the_five_hour_window() {
        let rl = RateLimits {
            windows: vec![
                RateWindow {
                    key: "seven_day".to_string(),
                    label: "7-day".to_string(),
                    used_percent: 10.0,
                    resets_at: ts("2026-05-20T00:00:00Z"),
                },
                RateWindow {
                    key: "five_hour".to_string(),
                    label: "5-hour".to_string(),
                    used_percent: 42.0,
                    resets_at: ts("2026-05-15T18:00:00Z"),
                },
            ],
        };
        let w = rl.five_hour().expect("five_hour present");
        assert_eq!(w.used_percent, 42.0);
    }

    #[test]
    fn five_hour_is_none_when_absent() {
        let rl = RateLimits {
            windows: vec![RateWindow {
                key: "seven_day".to_string(),
                label: "7-day".to_string(),
                used_percent: 10.0,
                resets_at: ts("2026-05-20T00:00:00Z"),
            }],
        };
        assert!(rl.five_hour().is_none());
    }

    #[test]
    fn seven_day_returns_the_seven_day_window() {
        let rl = RateLimits {
            windows: vec![RateWindow {
                key: "seven_day".to_string(),
                label: "7-day".to_string(),
                used_percent: 88.0,
                resets_at: ts("2026-05-20T00:00:00Z"),
            }],
        };
        let w = rl.seven_day().expect("seven_day present");
        assert_eq!(w.used_percent, 88.0);
    }

    #[test]
    fn seven_day_is_none_when_absent() {
        let rl = RateLimits { windows: vec![] };
        assert!(rl.seven_day().is_none());
    }

    #[test]
    fn an_unknown_key_window_is_reachable_only_via_windows() {
        // Windows beyond five_hour/seven_day have no named accessor - they're
        // only reachable via the generic `windows` list. Pins that the
        // accessors don't accidentally act as a filter that hides them.
        let rl = RateLimits {
            windows: vec![RateWindow {
                key: "seven_day_fable".to_string(),
                label: "Seven Day Fable".to_string(),
                used_percent: 0.0,
                resets_at: ts("2026-07-07T23:00:00Z"),
            }],
        };
        assert!(rl.five_hour().is_none());
        assert!(rl.seven_day().is_none());
        assert_eq!(rl.windows.len(), 1);
        assert_eq!(rl.windows[0].key, "seven_day_fable");
    }
}
