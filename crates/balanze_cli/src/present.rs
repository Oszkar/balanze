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

/// Map a utilization fraction (0.0..=1.0+, may exceed 1.0 on overage) to a
/// color bucket via the shared `window::Severity` classifier, so the CLI matrix
/// agrees with the tray, popover, and statusline at 50 / 75 / 90.
pub(crate) fn bucket_for_fraction(used: f64) -> Bucket {
    match window::Severity::from_util((used * 100.0) as f32) {
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
    match ratio {
        None => Bucket::Neutral,
        Some(r) if r > 1.5 => Bucket::Critical,
        Some(r) if r >= 1.0 => Bucket::Warn,
        Some(_) => Bucket::Ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_for_fraction_matches_severity_bands() {
        // The CLI matrix inherits the shared `window::Severity` scale
        // (Green/Yellow/Orange/Red at 50 / 75 / 90, inclusive `>=`); a change to
        // those cutoffs flows here automatically, no manual cross-crate sync.
        assert_eq!(bucket_for_fraction(0.0), Bucket::Ok);
        assert_eq!(bucket_for_fraction(0.499), Bucket::Ok);
        assert_eq!(bucket_for_fraction(0.50), Bucket::Warn);
        assert_eq!(bucket_for_fraction(0.749), Bucket::Warn);
        assert_eq!(bucket_for_fraction(0.75), Bucket::Orange);
        assert_eq!(bucket_for_fraction(0.899), Bucket::Orange);
        assert_eq!(bucket_for_fraction(0.90), Bucket::Critical);
        assert_eq!(bucket_for_fraction(1.25), Bucket::Critical);
    }

    #[test]
    fn bucket_for_pace_ratio_none_is_neutral() {
        assert_eq!(bucket_for_pace_ratio(None), Bucket::Neutral);
    }

    #[test]
    fn bucket_for_pace_ratio_buckets_by_burn() {
        assert_eq!(bucket_for_pace_ratio(Some(0.5)), Bucket::Ok);
        assert_eq!(bucket_for_pace_ratio(Some(0.999)), Bucket::Ok);
        assert_eq!(bucket_for_pace_ratio(Some(1.0)), Bucket::Warn);
        assert_eq!(bucket_for_pace_ratio(Some(1.5)), Bucket::Warn);
        assert_eq!(bucket_for_pace_ratio(Some(1.51)), Bucket::Critical);
        assert_eq!(bucket_for_pace_ratio(Some(3.0)), Bucket::Critical);
    }
}
