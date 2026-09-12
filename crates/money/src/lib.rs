//! The single currency display policy.
//!
//! Money is `i64` micro-USD everywhere internally (AGENTS.md §2.1); this crate
//! owns the one place it becomes something a human reads. Every Rust surface
//! goes through here so the CLI, the statusline, and the TUI cannot round the
//! same amount differently.
//!
//! The policy is **integer** rounding to whole cents, half away from zero. It
//! is deliberately not `format!("{:.2}", micro as f64 / 1_000_000.0)`: that
//! rounds the exact binary double, so 1_005_000 micro-USD (`1.005`, stored as
//! `1.00499999...`) prints `$1.00`, while the frontend's `Intl.NumberFormat`
//! rounds the shortest decimal and prints `$1.01`. Rounding the integer first
//! removes the disagreement at its source.
//!
//! The frontend cannot share this code, so the same policy is pinned for both
//! languages by the `money` vectors in `tests/fixtures/presentation-policy.json`,
//! which this crate and `src/lib/presentation/format.test.ts` both read.
//! Grouping is NOT part of the shared policy: the frontend uses
//! `Intl.NumberFormat`, which adds thousands separators, and these surfaces do
//! not. Only the cent value is contractual.

/// Micro-USD in one cent.
const MICRO_PER_CENT: u64 = 10_000;

/// Round micro-USD to whole cents, half away from zero.
///
/// Half away from zero (not banker's rounding) is what a reader expects from a
/// money figure and what the frontend's formatter does, and the two have to
/// agree.
pub fn micro_usd_to_cents(micro: i64) -> i64 {
    // `unsigned_abs` so `i64::MIN` has no special case, and the +half cannot
    // overflow: `u64::MAX` is far above `i64::MAX + 5_000`.
    let cents = (micro.unsigned_abs() + MICRO_PER_CENT / 2) / MICRO_PER_CENT;
    // Bounded by `i64::MAX / 10_000`, so the cast is always exact.
    let cents = cents as i64;
    if micro < 0 { -cents } else { cents }
}

/// Format micro-USD as `$X.XX`, with the sign ahead of the symbol (`-$1.01`)
/// so it reads the way the frontend's formatter renders the same amount.
///
/// An amount that rounds to zero never prints as negative zero.
pub fn micro_usd_to_display(micro: i64) -> String {
    let cents = micro_usd_to_cents(micro);
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    format!("{sign}${}.{:02}", abs / 100, abs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct MoneyPolicy {
        money: Vec<MoneyCase>,
    }

    #[derive(serde::Deserialize)]
    struct MoneyCase {
        #[serde(rename = "microUsd")]
        micro_usd: i64,
        cents: i64,
    }

    #[test]
    fn matches_the_shared_cent_rounding_vectors() {
        // The same vectors drive `src/lib/presentation/format.test.ts`. If this
        // fails there, the two languages have drifted apart again.
        let policy: MoneyPolicy = serde_json::from_str(include_str!(
            "../../../tests/fixtures/presentation-policy.json"
        ))
        .expect("shared presentation-policy fixture parses");
        assert!(!policy.money.is_empty(), "the fixture must carry vectors");

        for case in policy.money {
            assert_eq!(
                micro_usd_to_cents(case.micro_usd),
                case.cents,
                "{} micro-USD",
                case.micro_usd
            );
        }
    }

    #[test]
    fn half_a_cent_rounds_away_from_zero_not_down() {
        // The regression: the float path printed $1.00 here because 1.005 is
        // stored as 1.00499999..., while the frontend printed $1.01.
        assert_eq!(micro_usd_to_display(1_005_000), "$1.01");
        assert_eq!(micro_usd_to_display(1_004_999), "$1.00");
        assert_eq!(micro_usd_to_display(1_015_000), "$1.02");
    }

    #[test]
    fn display_pads_cents_and_places_the_sign_before_the_symbol() {
        assert_eq!(micro_usd_to_display(0), "$0.00");
        assert_eq!(micro_usd_to_display(5_000), "$0.01");
        assert_eq!(micro_usd_to_display(1_000_000), "$1.00");
        assert_eq!(micro_usd_to_display(1_100_000), "$1.10");
        assert_eq!(micro_usd_to_display(12_345_678), "$12.35");
        assert_eq!(micro_usd_to_display(-1_005_000), "-$1.01");
    }

    #[test]
    fn an_amount_that_rounds_to_zero_is_never_negative_zero() {
        assert_eq!(micro_usd_to_display(-1), "$0.00");
        assert_eq!(micro_usd_to_display(-4_999), "$0.00");
    }

    #[test]
    fn extreme_values_neither_panic_nor_wrap() {
        // `unsigned_abs` is why i64::MIN needs no special case, and the
        // half-cent addend cannot overflow the u64 it is added to.
        assert_eq!(micro_usd_to_cents(i64::MAX), 922_337_203_685_478);
        assert_eq!(micro_usd_to_cents(i64::MIN), -922_337_203_685_478);
    }
}
