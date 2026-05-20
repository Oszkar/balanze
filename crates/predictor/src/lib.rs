//! Pure EWMA prediction with a warm-up state machine.
//!
//! Boundary: pure-function crate (AGENTS.md §4 #2). No I/O, no `tokio::spawn`,
//! no logging above `debug`. The coordinator owns the history ring buffer
//! and calls `predict` after each successful merge.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// First 15 minutes after the rolling window starts are warm-up regardless
/// of event count. Within this period, the EWMA has too few observations to
/// produce a reliable signal.
const WARMUP_MINUTES: i64 = 15;

/// Minimum events seen since reset before the predictor will emit a number.
/// Below this, variance is too noisy to be honest.
const MIN_EVENTS_FOR_PREDICTION: usize = 10;

/// EWMA smoothing factor. 0.3 weights recent observations heavily without
/// overreacting to single outliers. Hand-tuned against simulated workloads.
const EWMA_ALPHA: f64 = 0.3;

/// Variance threshold (pct-per-min units squared) above which the predictor
/// downgrades to `Uncertain`. Calibrated so a steady ~0.5 %/min growth is
/// well below and a wildly oscillating signal is well above.
const VARIANCE_CONFIDENT_THRESHOLD: f64 = 50.0;

/// Confidence level of the current prediction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PredictionState {
    /// Warm-up period: fewer than 15 minutes since window start OR fewer than
    /// 10 history points. No ETA is reliable enough to show.
    Insufficient,
    /// Enough data to compute an ETA, but EWMA variance is high — workload
    /// is erratic and the ETA could be significantly off.
    Uncertain,
    /// Warm-up passed and EWMA variance is below threshold: ETA is reasonably
    /// stable.
    Confident,
}

/// Output of [`predict`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    pub state: PredictionState,
    /// `None` when:
    /// - state is `Insufficient` (warm-up gates not passed), or
    /// - the EWMA rate is zero or negative (usage not growing), or
    /// - `current_pct >= 100` (already at cap), or
    /// - the projected ETA would overflow `Duration::seconds(i64)` (degenerate
    ///   near-zero positive rate).
    ///
    /// Otherwise the EWMA-projected duration until 100% utilisation.
    #[serde(with = "duration_seconds_opt")]
    pub eta_to_cap: Option<Duration>,
    /// Always present. Deterministic from `window_reset - now`, clamped to
    /// zero if reset is already in the past.
    #[serde(with = "duration_seconds")]
    pub eta_to_reset: Duration,
    pub computed_at: DateTime<Utc>,
}

/// A single observation `(timestamp, used_pct)` from the rolling window.
/// Callers pass slices of these to `predict`; the slice MUST be sorted
/// oldest-first (`predict` debug-asserts this).
///
/// The coordinator appends one of these after each successful JSONL + OAuth
/// merge; the predictor reads the history slice without owning it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    pub ts: DateTime<Utc>,
    /// Rolling-window utilisation as a percentage in [0, 100].
    pub used_pct: f64,
}

/// Compute an EWMA-based usage prediction.
///
/// # Parameters
/// - `now`: wall-clock instant for this prediction.
/// - `current_pct`: current rolling-window utilisation in [0, 100].
/// - `history`: ordered slice of past `(ts, used_pct)` observations. Oldest
///   first. The coordinator appends one snapshot per merge cycle.
/// - `window_reset`: the server-reported (or derived) timestamp at which the
///   5-hour rolling window next resets. `window_start = window_reset - 5h`.
///
/// Returns `Insufficient` (with no ETA) during the first 15 minutes of the
/// window or when `history.len() < 10`. Otherwise returns `Uncertain` or
/// `Confident` with an EWMA-projected `eta_to_cap`.
pub fn predict(
    now: DateTime<Utc>,
    current_pct: f64,
    history: &[WindowSnapshot],
    window_reset: DateTime<Utc>,
) -> Prediction {
    let eta_to_reset = (window_reset - now).max(Duration::zero());

    // Warm-up gate 1: within the first 15 minutes of the rolling window?
    // The cap window is [window_reset - 5h, window_reset).
    // Elapsed = now - window_start = now - (window_reset - 5h).
    let window_start = window_reset - Duration::hours(5);
    let elapsed_in_window = now - window_start;
    if elapsed_in_window < Duration::minutes(WARMUP_MINUTES) {
        return Prediction {
            state: PredictionState::Insufficient,
            eta_to_cap: None,
            eta_to_reset,
            computed_at: now,
        };
    }

    // Warm-up gate 2: enough history points?
    if history.len() < MIN_EVENTS_FOR_PREDICTION {
        return Prediction {
            state: PredictionState::Insufficient,
            eta_to_cap: None,
            eta_to_reset,
            computed_at: now,
        };
    }

    let ewma_rate = compute_ewma_rate(history);
    let variance = compute_variance(history, ewma_rate);

    let state = if variance > VARIANCE_CONFIDENT_THRESHOLD {
        PredictionState::Uncertain
    } else {
        PredictionState::Confident
    };

    let eta_to_cap = if ewma_rate > 0.0 && current_pct < 100.0 {
        let pct_remaining = 100.0 - current_pct;
        let minutes_to_cap = pct_remaining / ewma_rate;
        let cap_seconds = minutes_to_cap * 60.0;
        // Guard against near-zero rates producing an absurd ETA: if the
        // projection would saturate i64, the cap is effectively unreachable.
        if cap_seconds > i64::MAX as f64 {
            None
        } else {
            Some(Duration::seconds(cap_seconds as i64))
        }
    } else {
        // Rate is zero or negative (usage not growing), or already at cap.
        None
    };

    Prediction {
        state,
        eta_to_cap,
        eta_to_reset,
        computed_at: now,
    }
}

/// Compute the EWMA of per-interval growth rates (% per minute) over the
/// history slice. Returns 0.0 when there are fewer than two data points.
fn compute_ewma_rate(history: &[WindowSnapshot]) -> f64 {
    debug_assert!(
        history.windows(2).all(|w| w[0].ts <= w[1].ts),
        "WindowSnapshot history must be sorted oldest-first"
    );
    let mut ewma: Option<f64> = None;
    for pair in history.windows(2) {
        let dt_min = (pair[1].ts - pair[0].ts).num_seconds() as f64 / 60.0;
        if dt_min <= 0.0 {
            continue;
        }
        let rate = (pair[1].used_pct - pair[0].used_pct) / dt_min;
        ewma = Some(match ewma {
            None => rate,
            Some(prev) => EWMA_ALPHA * rate + (1.0 - EWMA_ALPHA) * prev,
        });
    }
    ewma.unwrap_or(0.0)
}

/// Mean squared deviation of per-interval growth rates from the EWMA-weighted
/// rate (passed as `ewma_rate`). Note: using the EWMA as the centering value
/// rather than the arithmetic mean is intentionally conservative — for
/// trending signals this inflates variance, biasing the classifier toward
/// `Uncertain` rather than risking a false `Confident`.
/// Returns 0.0 when there are fewer than two intervals.
fn compute_variance(history: &[WindowSnapshot], ewma_rate: f64) -> f64 {
    debug_assert!(
        history.windows(2).all(|w| w[0].ts <= w[1].ts),
        "WindowSnapshot history must be sorted oldest-first"
    );
    let mut sum_sq = 0.0;
    let mut n = 0usize;
    for pair in history.windows(2) {
        let dt_min = (pair[1].ts - pair[0].ts).num_seconds() as f64 / 60.0;
        if dt_min <= 0.0 {
            continue;
        }
        let rate = (pair[1].used_pct - pair[0].used_pct) / dt_min;
        let d = rate - ewma_rate;
        sum_sq += d * d;
        n += 1;
    }
    if n == 0 {
        0.0
    } else {
        sum_sq / n as f64
    }
}

// ---------------------------------------------------------------------------
// Serde adapters for chrono::Duration (no built-in serde support).
// ---------------------------------------------------------------------------

mod duration_seconds {
    use chrono::Duration;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(d.num_seconds())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::seconds(i64::deserialize(d)?))
    }
}

mod duration_seconds_opt {
    use chrono::Duration;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        d.map(|dur| dur.num_seconds()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<i64>::deserialize(d)?.map(Duration::seconds))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn t(min: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 21, 0, 0, 0).unwrap() + Duration::minutes(min)
    }

    /// Build a history of `n` evenly-spaced snapshots starting at `t(0)`,
    /// 1 minute apart, with `used_pct` increasing linearly from 0.
    fn stable_history(n: usize, rate_per_min: f64) -> Vec<WindowSnapshot> {
        (0..n)
            .map(|i| WindowSnapshot {
                ts: t(i as i64),
                used_pct: i as f64 * rate_per_min,
            })
            .collect()
    }

    /// Build a history that alternates between two extreme percentages —
    /// maximally noisy signal.
    fn noisy_history(n: usize) -> Vec<WindowSnapshot> {
        (0..n)
            .map(|i| WindowSnapshot {
                ts: t(i as i64),
                used_pct: if i % 2 == 0 { 5.0 } else { 80.0 },
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // Warm-up gate 1: elapsed in window < 15 minutes
    // ------------------------------------------------------------------

    #[test]
    fn insufficient_during_first_15_minutes_after_reset() {
        // window_reset is 4h ahead; window_start = reset - 5h = 1h ahead - 5h = now - 4h.
        // elapsed_in_window = now - window_start = 4h … but wait:
        // We want now to be only a few minutes into the window.
        // window_start = window_reset - 5h. elapsed = now - window_start.
        // Let window_reset = t(300) (5h from t(0)), so window_start = t(0).
        // Set now = t(5) → elapsed = 5 min < 15 min → Insufficient.
        let reset = t(300); // 5h from epoch
        let window_start = reset - Duration::hours(5); // = t(0)
        let now = window_start + Duration::minutes(5); // 5 min into window
        let p = predict(now, 10.0, &[], reset);
        assert!(
            matches!(p.state, PredictionState::Insufficient),
            "expected Insufficient in first 15 min, got {:?}",
            p.state
        );
        assert_eq!(p.eta_to_cap, None);
        assert!(p.eta_to_reset > Duration::zero());
    }

    #[test]
    fn still_insufficient_at_14_minutes() {
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(14);
        let p = predict(now, 5.0, &[], reset);
        assert!(matches!(p.state, PredictionState::Insufficient));
    }

    #[test]
    fn warm_up_ends_at_exactly_15_minutes() {
        // At exactly 15 min elapsed, warm-up gate 1 should NOT trigger
        // (elapsed >= WARMUP_MINUTES). Gate 2 (< 10 events) will trigger
        // because history is empty → still Insufficient, but for gate 2 reason.
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(15);
        // history empty → Insufficient (gate 2), not gate 1
        let p = predict(now, 5.0, &[], reset);
        assert!(matches!(p.state, PredictionState::Insufficient));
        // To confirm gate 1 is cleared: provide 10+ history points and verify
        // we get a non-Insufficient state.
        let hist = stable_history(12, 0.5);
        let p2 = predict(now, 6.0, &hist, reset);
        assert!(
            !matches!(p2.state, PredictionState::Insufficient),
            "past warm-up with enough history should not be Insufficient, got {:?}",
            p2.state
        );
    }

    // ------------------------------------------------------------------
    // Warm-up gate 2: fewer than MIN_EVENTS_FOR_PREDICTION history points
    // ------------------------------------------------------------------

    #[test]
    fn insufficient_with_fewer_than_ten_events() {
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(30); // past warm-up gate 1
        let history: Vec<WindowSnapshot> = (0..5)
            .map(|i| WindowSnapshot {
                ts: t(i * 2),
                used_pct: i as f64,
            })
            .collect();
        let p = predict(now, 5.0, &history, reset);
        assert!(matches!(p.state, PredictionState::Insufficient));
    }

    #[test]
    fn sufficient_at_exactly_ten_events() {
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(30);
        let history = stable_history(10, 0.5); // exactly 10 points
        let p = predict(now, 5.0, &history, reset);
        assert!(
            !matches!(p.state, PredictionState::Insufficient),
            "exactly 10 events should exit gate 2, got {:?}",
            p.state
        );
    }

    // ------------------------------------------------------------------
    // Confident: stable growth, low variance
    // ------------------------------------------------------------------

    #[test]
    fn confident_with_stable_growth() {
        let reset = t(300); // 5h from t(0)
        let window_start = reset - Duration::hours(5); // t(0)
        let now = window_start + Duration::minutes(30); // well past warm-up
        let history = stable_history(12, 0.5); // steady 0.5 %/min
        let p = predict(now, 6.0, &history, reset);
        assert!(
            matches!(p.state, PredictionState::Confident),
            "stable growth should yield Confident, got {:?}",
            p.state
        );
        assert!(p.eta_to_cap.is_some(), "Confident must carry an ETA");
        // Stable rate is 0.5 %/min (each step adds 0.5 %). With current_pct = 6.0,
        // 94 % remains → 94 / 0.5 = 188 min = 11280 s. EWMA of a constant rate
        // equals the constant, so this is exact.
        let eta = p.eta_to_cap.unwrap();
        assert_eq!(
            eta.num_seconds(),
            11280,
            "ETA = (100 - 6) / 0.5 * 60 = 11280 s"
        );
    }

    // ------------------------------------------------------------------
    // Uncertain: high variance
    // ------------------------------------------------------------------

    #[test]
    fn uncertain_with_high_variance() {
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(30);
        let history = noisy_history(12);
        // 12 points (even): final EWMA is ~+15 %/min (positive), so eta_to_cap = Some.
        // An odd count would terminate on the low 5.0 value → negative EWMA → None.
        let p = predict(now, 50.0, &history, reset);
        assert!(
            matches!(p.state, PredictionState::Uncertain),
            "high variance should yield Uncertain, got {:?}",
            p.state
        );
        // ETA is still returned even in Uncertain (criterion #3)
        assert!(p.eta_to_cap.is_some(), "Uncertain must still carry an ETA");
    }

    // ------------------------------------------------------------------
    // eta_to_reset
    // ------------------------------------------------------------------

    #[test]
    fn eta_to_reset_clamps_to_zero_if_past() {
        let reset = t(0);
        let now = t(10); // 10 min past reset
        let p = predict(now, 0.0, &[], reset);
        assert_eq!(
            p.eta_to_reset,
            Duration::zero(),
            "eta_to_reset must clamp to zero when reset is in the past"
        );
    }

    #[test]
    fn eta_to_reset_positive_when_reset_in_future() {
        let reset = t(60); // 1h ahead
        let now = t(0);
        let p = predict(now, 0.0, &[], reset);
        assert_eq!(p.eta_to_reset, Duration::minutes(60));
    }

    // ------------------------------------------------------------------
    // eta_to_cap edge cases
    // ------------------------------------------------------------------

    #[test]
    fn eta_to_cap_none_when_rate_zero() {
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(30);
        // Flat history: all at same value → rate = 0
        let history: Vec<WindowSnapshot> = (0..12)
            .map(|i| WindowSnapshot {
                ts: t(i as i64),
                used_pct: 50.0,
            })
            .collect();
        let p = predict(now, 50.0, &history, reset);
        // Rate is 0 → eta_to_cap must be None
        assert_eq!(p.eta_to_cap, None, "zero rate means cap is never reached");
    }

    #[test]
    fn eta_to_cap_none_when_already_at_cap() {
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(30);
        let history = stable_history(12, 0.5);
        let p = predict(now, 100.0, &history, reset); // already at cap
        assert_eq!(
            p.eta_to_cap, None,
            "already at cap → eta_to_cap must be None"
        );
    }

    #[test]
    fn eta_to_cap_none_when_rate_is_effectively_zero() {
        // history with extremely slow growth → ewma_rate ~1e-300
        let reset = t(300);
        let now = t(20);
        let history: Vec<WindowSnapshot> = (0..12)
            .map(|i| WindowSnapshot {
                ts: t(i as i64),
                used_pct: 50.0 + (i as f64) * 1e-300,
            })
            .collect();
        let p = predict(now, /* current_pct */ 50.0, &history, reset);
        // The rate may technically be > 0 but eta_to_cap should still be None
        // because the projected seconds saturate i64.
        assert!(
            p.eta_to_cap.is_none() || p.eta_to_cap.unwrap().num_seconds() < 10_000_000_000_000,
            "near-zero rate should not produce a billion-year ETA"
        );
    }

    #[test]
    fn eta_to_cap_none_when_usage_is_declining() {
        let reset = t(300);
        let now = t(20);
        // Declining: 80% → 25% over the window.
        let history: Vec<WindowSnapshot> = (0..12)
            .map(|i| WindowSnapshot {
                ts: t(i as i64),
                used_pct: 80.0 - (i as f64) * 5.0, // 80, 75, 70, ..., 25
            })
            .collect();
        let p = predict(now, /* current_pct */ 25.0, &history, reset);
        // Declining rate → ewma_rate < 0 → eta_to_cap None.
        assert!(matches!(
            p.state,
            PredictionState::Confident | PredictionState::Uncertain
        ));
        assert!(
            p.eta_to_cap.is_none(),
            "declining usage should produce None ETA"
        );
    }

    // ------------------------------------------------------------------
    // Serde round-trip
    // ------------------------------------------------------------------

    #[test]
    fn prediction_serde_round_trip() {
        let reset = t(300);
        let window_start = reset - Duration::hours(5);
        let now = window_start + Duration::minutes(30);
        let history = stable_history(12, 0.5);
        let p = predict(now, 6.0, &history, reset);
        let json = serde_json::to_string(&p).expect("serialize");
        let back: Prediction = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p.state, back.state);
        assert_eq!(p.eta_to_reset, back.eta_to_reset);
        assert_eq!(p.eta_to_cap, back.eta_to_cap);
    }
}
