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

use state_coordinator::{AnthropicQuotaSource, Snapshot};

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
}
