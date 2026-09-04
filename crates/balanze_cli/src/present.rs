//! Shared presentation helper: maps a utilization fraction or a pace ratio to
//! a color `Bucket`. Utilization coloring delegates to the shared
//! `window::Severity` classifier (crates/window/src/lib.rs) - the one
//! green/yellow/orange/red heat scale at 50 / 75 / 90 that the tray, popover,
//! and statusline also use - so the surfaces cannot drift apart. The tray's own
//! six-way `ColorBucket` (src-tauri/src/tauri_sink.rs) maps the same `Severity`
//! bands to its icon RGBA.
//!
//! Consumed by the colored one-shot `status` renderer and the `watch` TUI so
//! the matrix coloring logic is not forked.

use state_coordinator::{AnthropicQuotaSource, Snapshot, WindowPace};

/// Color bucket for a presented value. The four utilization heat bands mirror
/// `window::Severity` (Ok=Green, Warn=Yellow, Orange, Critical=Red), plus
/// `Neutral` for "no signal yet" (cold start / missing pace ratio). Pace-ratio
/// coloring reuses Ok/Warn/Critical only - a different axis, no Orange band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bucket {
    Ok,
    Warn,
    Orange,
    Critical,
    Neutral,
}

/// Truecolor RGB for the "orange" heat band, matching the tray icon
/// (`src-tauri/src/tray_icon.rs`). Shared by the compact matrix (`render.rs`)
/// and the `watch` TUI (`tui.rs`) so the two renderers cannot drift if the tray
/// color ever changes. The 16-color ANSI palette has no orange, hence truecolor.
pub(crate) const TRAY_ORANGE: (u8, u8, u8) = (0xd9, 0x6a, 0x2a);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnthropicSourceLabel {
    Statusline,
    OAuth,
}

impl AnthropicSourceLabel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Statusline => "statusline",
            Self::OAuth => "oauth",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnthropicWindow<'a> {
    pub(crate) key: &'a str,
    pub(crate) percent: f32,
}

/// The compact/TUI density rule: show the five-hour window plus the worst
/// non-five-hour cadence from the canonical fresh-statusline-first source.
pub(crate) fn anthropic_display_windows(
    s: &Snapshot,
) -> Option<(AnthropicSourceLabel, Vec<AnthropicWindow<'_>>, bool)> {
    fn select<'a>(windows: impl Iterator<Item = AnthropicWindow<'a>>) -> Vec<AnthropicWindow<'a>> {
        let mut five = None;
        let mut weekly = None;
        for window in windows {
            if window.key == "five_hour" {
                five.get_or_insert(window);
            } else if weekly
                .is_none_or(|current: AnthropicWindow<'_>| window.percent > current.percent)
            {
                weekly = Some(window);
            }
        }
        [five, weekly].into_iter().flatten().collect()
    }

    match s.anthropic_quota_source()? {
        AnthropicQuotaSource::Statusline {
            rate_limits: rl,
            stale,
        } => Some((
            AnthropicSourceLabel::Statusline,
            select(rl.windows.iter().map(|w| AnthropicWindow {
                key: &w.key,
                percent: w.used_percent,
            })),
            stale,
        )),
        AnthropicQuotaSource::OAuth {
            snapshot: oauth,
            stale,
        } => Some((
            AnthropicSourceLabel::OAuth,
            select(oauth.cadences.iter().map(|c| AnthropicWindow {
                key: &c.key,
                percent: c.utilization_percent,
            })),
            stale,
        )),
    }
}

/// Pace entries whose keys belong to the currently selected Anthropic quota
/// source. This prevents an OAuth fallback vector from being paired with a
/// statusline-only window family that did not produce pace.
pub(crate) fn matching_anthropic_pace(s: &Snapshot) -> Vec<&WindowPace> {
    match s.anthropic_quota_source() {
        Some(AnthropicQuotaSource::Statusline { rate_limits, .. }) => s
            .pace
            .iter()
            .filter(|pace| rate_limits.windows.iter().any(|w| w.key == pace.key))
            .collect(),
        Some(AnthropicQuotaSource::OAuth { snapshot, .. }) => s
            .pace
            .iter()
            .filter(|pace| snapshot.cadences.iter().any(|c| c.key == pace.key))
            .collect(),
        None => Vec::new(),
    }
}

/// Map a utilization fraction (0.0..=1.0+, may exceed 1.0 on overage) to a
/// color bucket via the shared `window::Severity` classifier, so the CLI matrix
/// agrees with the tray, popover, and statusline at 50 / 75 / 90.
pub(crate) fn bucket_for_fraction(used: f64) -> Bucket {
    let displayed_percent = (used * 100.0).round() as f32;
    match window::Severity::from_util(displayed_percent) {
        window::Severity::Green => Bucket::Ok,
        window::Severity::Yellow => Bucket::Warn,
        window::Severity::Orange => Bucket::Orange,
        window::Severity::Red => Bucket::Critical,
    }
}

/// Map a pace ratio (used% / elapsed%) to a bucket. `None` (no pace data) is
/// `Neutral`. Burning faster than the clock (> 1.0) is `Warn`; well over pace
/// (> 1.5) is `Critical`; at or under pace (< 1.0) is `Ok`.
pub(crate) fn bucket_for_pace_ratio(ratio: Option<f64>) -> Bucket {
    match window::PaceVerdict::from_ratio(ratio) {
        window::PaceVerdict::TooEarly => Bucket::Neutral,
        window::PaceVerdict::Critical => Bucket::Critical,
        window::PaceVerdict::Warn => Bucket::Warn,
        window::PaceVerdict::Under | window::PaceVerdict::OnPace => Bucket::Ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot};
    use chrono::{DateTime, Utc};
    use claude_statusline::{RateLimits, RateWindow, StatuslineFilePayload, StatuslineSnapshot};
    use codex_local::{CodexQuotaSnapshot, RateLimitWindow, WindowKind};
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PolicyFixture {
        anthropic: Vec<AnthropicCase>,
        codex: Vec<CodexCase>,
        severity: Vec<SeverityCase>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AnthropicCase {
        name: String,
        fetched_at: String,
        statusline: Option<StatuslineInput>,
        oauth: Option<OAuthInput>,
        expected: ExpectedAnthropic,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct StatuslineInput {
        captured_at: String,
        error: bool,
        windows: Vec<PolicyWindow>,
    }

    #[derive(Deserialize)]
    struct OAuthInput {
        error: bool,
        windows: Vec<PolicyWindow>,
    }

    #[derive(Deserialize)]
    struct PolicyWindow {
        key: String,
        percent: f32,
    }

    #[derive(Deserialize)]
    struct ExpectedAnthropic {
        source: String,
        stale: bool,
        windows: Vec<PolicyWindow>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CodexCase {
        name: String,
        fetched_at: String,
        windows: Vec<CodexWindowInput>,
        expected: ExpectedCodex,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CodexWindowInput {
        percent: f64,
        duration_minutes: u64,
        resets_at: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ExpectedCodex {
        worst_index: usize,
        five_index: Option<usize>,
        weekly_index: Option<usize>,
        expired: bool,
        labels: Vec<String>,
    }

    #[derive(Deserialize)]
    struct SeverityCase {
        percent: f64,
        expected: String,
    }

    fn policy_fixture() -> PolicyFixture {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/presentation-policy.json"
        ))
        .expect("shared presentation-policy fixture parses")
    }

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("policy timestamp is RFC 3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn bucket_for_fraction_matches_severity_bands() {
        // The status and TUI surfaces inherit the shared `window::Severity`
        // scale. Every quota surface classifies the rounded integer it presents,
        // so values half a point below a cutoff enter the next band too.
        let cases = [
            (0.494, Bucket::Ok),
            (0.495, Bucket::Warn),
            (0.744, Bucket::Warn),
            (0.745, Bucket::Orange),
            (0.894, Bucket::Orange),
            (0.895, Bucket::Critical),
            (1.25, Bucket::Critical),
        ];

        for (used, expected) in cases {
            assert_eq!(bucket_for_fraction(used), expected, "used={used}");
        }
    }

    #[test]
    fn bucket_for_pace_ratio_none_is_neutral() {
        assert_eq!(bucket_for_pace_ratio(None), Bucket::Neutral);
    }

    #[test]
    fn bucket_for_pace_ratio_buckets_by_burn() {
        assert_eq!(bucket_for_pace_ratio(Some(0.5)), Bucket::Ok);
        assert_eq!(bucket_for_pace_ratio(Some(1.0)), Bucket::Ok);
        assert_eq!(bucket_for_pace_ratio(Some(1.11)), Bucket::Ok);
        assert_eq!(bucket_for_pace_ratio(Some(1.12)), Bucket::Warn);
        assert_eq!(bucket_for_pace_ratio(Some(1.49)), Bucket::Warn);
        assert_eq!(bucket_for_pace_ratio(Some(1.5)), Bucket::Critical);
        assert_eq!(bucket_for_pace_ratio(Some(3.0)), Bucket::Critical);
    }

    #[test]
    fn shared_anthropic_policy_vectors_match_rust() {
        for case in policy_fixture().anthropic {
            let fetched_at = timestamp(&case.fetched_at);
            let resets_at = timestamp("2026-07-15T10:00:00Z");
            let mut snapshot = Snapshot::empty(fetched_at);
            if let Some(statusline) = case.statusline {
                snapshot.claude_statusline = Some(StatuslineFilePayload::new(
                    StatuslineSnapshot {
                        rate_limits: Some(RateLimits {
                            windows: statusline
                                .windows
                                .into_iter()
                                .map(|w| RateWindow {
                                    label: w.key.clone(),
                                    key: w.key,
                                    used_percent: w.percent,
                                    resets_at,
                                })
                                .collect(),
                        }),
                        session_cost_micro_usd: None,
                        claude_code_version: None,
                        model_display_name: None,
                        context_used_percent: None,
                    },
                    timestamp(&statusline.captured_at),
                ));
                if statusline.error {
                    snapshot.claude_statusline_error = Some("reader failed".to_string());
                }
            }
            if let Some(oauth) = case.oauth {
                snapshot.claude_oauth = Some(ClaudeOAuthSnapshot {
                    cadences: oauth
                        .windows
                        .into_iter()
                        .map(|w| CadenceBar {
                            display_label: w.key.clone(),
                            key: w.key,
                            utilization_percent: w.percent,
                            resets_at,
                        })
                        .collect(),
                    extra_usage: None,
                    subscription_type: None,
                    rate_limit_tier: None,
                    org_uuid: None,
                    fetched_at,
                });
                if oauth.error {
                    snapshot.claude_oauth_error = Some("refresh failed".to_string());
                }
            }

            let (source, windows, stale) = anthropic_display_windows(&snapshot)
                .unwrap_or_else(|| panic!("{}: expected a selected source", case.name));
            assert_eq!(source.as_str(), case.expected.source, "{}", case.name);
            assert_eq!(stale, case.expected.stale, "{}", case.name);
            let actual: Vec<(&str, f32)> = windows.iter().map(|w| (w.key, w.percent)).collect();
            let expected: Vec<(&str, f32)> = case
                .expected
                .windows
                .iter()
                .map(|w| (w.key.as_str(), w.percent))
                .collect();
            assert_eq!(actual, expected, "{}", case.name);
        }
    }

    #[test]
    fn shared_codex_policy_vectors_match_rust() {
        for case in policy_fixture().codex {
            let fetched_at = timestamp(&case.fetched_at);
            let windows: Vec<RateLimitWindow> = case
                .windows
                .iter()
                .map(|w| RateLimitWindow {
                    used_percent: w.percent,
                    window_duration_minutes: w.duration_minutes,
                    resets_at: timestamp(&w.resets_at),
                })
                .collect();
            let snapshot = CodexQuotaSnapshot {
                observed_at: fetched_at,
                session_id: "policy".to_string(),
                primary: windows[0].clone(),
                secondary: windows.get(1).cloned(),
                plan_type: "pro".to_string(),
                rate_limit_reached: false,
                tokens: None,
                credits: None,
            };
            let index_of = |window: &RateLimitWindow| {
                snapshot
                    .windows()
                    .position(|candidate| std::ptr::eq(candidate, window))
            };
            assert_eq!(
                index_of(snapshot.worst_window().expect("primary always exists")),
                Some(case.expected.worst_index),
                "{}",
                case.name
            );
            assert_eq!(
                snapshot.five_hour().and_then(index_of),
                case.expected.five_index,
                "{}",
                case.name
            );
            assert_eq!(
                snapshot.weekly_or_other().and_then(index_of),
                case.expected.weekly_index,
                "{}",
                case.name
            );
            assert_eq!(
                snapshot.any_window_expired(fetched_at),
                case.expected.expired,
                "{}",
                case.name
            );
            let labels: Vec<&str> = snapshot
                .windows()
                .map(|w| match w.kind() {
                    WindowKind::FiveHour => "5h",
                    WindowKind::Weekly => "7d",
                    WindowKind::Other => "window",
                })
                .collect();
            assert_eq!(labels, case.expected.labels, "{}", case.name);
        }
    }

    #[test]
    fn shared_severity_policy_vectors_match_rust() {
        for case in policy_fixture().severity {
            let actual = match bucket_for_fraction(case.percent / 100.0) {
                Bucket::Ok => "green",
                Bucket::Warn => "yellow",
                Bucket::Orange => "orange",
                Bucket::Critical => "red",
                Bucket::Neutral => "neutral",
            };
            assert_eq!(actual, case.expected, "{}%", case.percent);
        }
    }
}
