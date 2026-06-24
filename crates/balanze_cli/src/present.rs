//! Shared presentation helper: maps a utilization fraction or a pace ratio to
//! a color `Bucket`, using the SAME thresholds as the tray gauge. The tray's
//! own `ColorBucket` lives in `src-tauri/src/tauri_sink.rs` and is `pub(crate)`
//! to that binary, so it cannot be imported here (the CLI does not depend on
//! the `src-tauri` package). The thresholds (50 / 90 percent, inclusive `>=`)
//! are replicated below and pinned to the documented tray values by
//! `bucket_for_fraction_matches_tray_thresholds`. Keep the two in lockstep:
//! if `tauri_sink.rs` QUOTA_WARN_PCT / QUOTA_BAD_PCT change, update here too.
//!
//! Consumed by the colored one-shot `status` renderer (and, later, the `watch`
//! TUI) so the matrix coloring logic is not forked.

/// Color bucket for a presented value. Collapsed from the tray's six-way
/// `ColorBucket` to the three signal states the CLI text surface needs, plus
/// `Neutral` for "no signal yet" (cold start / missing pace ratio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bucket {
    Ok,
    Warn,
    Critical,
    Neutral,
}

/// Tray-parity utilization thresholds, as FRACTIONS (the tray uses percent).
/// 0.50 -> Warn boundary (tray QUOTA_WARN_PCT = 50.0), 0.90 -> Critical
/// boundary (tray QUOTA_BAD_PCT = 90.0). Boundaries are inclusive (`>=`),
/// matching `ColorBucket::from_util` in src-tauri/src/tauri_sink.rs.
const WARN_FRACTION: f64 = 0.50;
const CRITICAL_FRACTION: f64 = 0.90;

/// Map a utilization fraction (0.0..=1.0+, may exceed 1.0 on overage) to a
/// color bucket using the same thresholds as the tray gauge.
pub(crate) fn bucket_for_fraction(used: f64) -> Bucket {
    if used >= CRITICAL_FRACTION {
        Bucket::Critical
    } else if used >= WARN_FRACTION {
        Bucket::Warn
    } else {
        Bucket::Ok
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
    fn bucket_for_fraction_matches_tray_thresholds() {
        // Boundaries mirror src-tauri/src/tauri_sink.rs ColorBucket::from_util
        // (QUOTA_WARN_PCT = 50.0, QUOTA_BAD_PCT = 90.0, inclusive `>=`),
        // expressed here as fractions. Any drift from the tray must fail.
        // The tray's intermediate 75% Orange band (QUOTA_ORANGE_PCT) is
        // intentionally folded into `Warn` here - the CLI uses a 3-way bucket,
        // so 50 / 90 are the only parity points, not an oversight.
        assert_eq!(bucket_for_fraction(0.0), Bucket::Ok);
        assert_eq!(bucket_for_fraction(0.499), Bucket::Ok);
        assert_eq!(bucket_for_fraction(0.50), Bucket::Warn);
        assert_eq!(bucket_for_fraction(0.899), Bucket::Warn);
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
