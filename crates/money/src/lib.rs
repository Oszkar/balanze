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
//!
//! There are two grains, one rounding rule. Cents is the default. The finer
//! ten-thousandths grain exists for per-line-item breakdowns, whose rows are
//! routinely sub-cent and would otherwise all render `$0.00`; it has no
//! frontend counterpart, so it is tested here rather than in the shared
//! fixture.

/// Micro-USD in one cent.
const MICRO_PER_CENT: u64 = 10_000;
/// Micro-USD in one ten-thousandth of a dollar, the grain the per-line-item
/// breakdowns need.
const MICRO_PER_SUB_CENT: u64 = 100;

/// Round micro-USD to whole units of `micro_per_unit`, half away from zero.
///
/// Half away from zero (not banker's rounding) is what a reader expects from a
/// money figure and what the frontend's formatter does, and the two have to
/// agree.
fn round_to_units(micro: i64, micro_per_unit: u64) -> i64 {
    // `unsigned_abs` so `i64::MIN` has no special case, and the +half cannot
    // overflow: `u64::MAX` is far above `i64::MAX + 5_000`.
    let units = (micro.unsigned_abs() + micro_per_unit / 2) / micro_per_unit;
    // Bounded by `i64::MAX / micro_per_unit`, so the cast is always exact.
    let units = units as i64;
    if micro < 0 { -units } else { units }
}

/// Render a rounded unit count as `$X.Y`, with the sign ahead of the symbol
/// (`-$1.01`) so it reads the way the frontend's formatter renders the same
/// amount. An amount that rounds to zero never prints as negative zero.
fn display_units(units: i64, units_per_dollar: u64, decimals: usize) -> String {
    let sign = if units < 0 { "-" } else { "" };
    let abs = units.unsigned_abs();
    format!(
        "{sign}${}.{:0decimals$}",
        abs / units_per_dollar,
        abs % units_per_dollar
    )
}

/// Round micro-USD to whole cents, half away from zero.
pub fn micro_usd_to_cents(micro: i64) -> i64 {
    round_to_units(micro, MICRO_PER_CENT)
}

/// Format micro-USD as `$X.XX`. The default for anything a reader sees as an
/// amount of money; this is the grain the frontend shares.
pub fn micro_usd_to_display(micro: i64) -> String {
    display_units(micro_usd_to_cents(micro), 100, 2)
}

/// Format micro-USD as `$X.XXXX`, for per-line-item breakdowns where cent
/// rounding would collapse the small rows into an undifferentiated `$0.00`.
///
/// Real data makes the case: a month's OpenAI line items run to
/// `$0.0868` / `$0.0094` / `$0.0021`, which cent rounding renders as
/// `$0.09` / `$0.01` / `$0.00`. Same integer rounding rule, finer grain - NOT
/// a float escape hatch, which is what this replaced.
pub fn micro_usd_to_display_precise(micro: i64) -> String {
    display_units(round_to_units(micro, MICRO_PER_SUB_CENT), 10_000, 4)
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
    fn the_precise_grain_keeps_sub_cent_rows_distinguishable() {
        // Cent rounding renders these as $0.09 / $0.01 / $0.00, losing the
        // smallest rows entirely. These are real OpenAI line-item amounts.
        assert_eq!(micro_usd_to_display_precise(86_784), "$0.0868");
        assert_eq!(micro_usd_to_display_precise(9_366), "$0.0094");
        assert_eq!(micro_usd_to_display_precise(2_089), "$0.0021");
    }

    #[test]
    fn the_precise_grain_rounds_by_the_same_rule() {
        // Half away from zero at the finer grain too - the float path rounded
        // the exact binary double instead, and disagreed here.
        assert_eq!(micro_usd_to_display_precise(150), "$0.0002");
        assert_eq!(micro_usd_to_display_precise(149), "$0.0001");
        assert_eq!(micro_usd_to_display_precise(0), "$0.0000");
        assert_eq!(micro_usd_to_display_precise(1_005_000), "$1.0050");
        assert_eq!(micro_usd_to_display_precise(-150), "-$0.0002");
        assert_eq!(micro_usd_to_display_precise(-49), "$0.0000");
    }

    #[test]
    fn both_grains_agree_on_the_amounts_they_can_both_express() {
        for micro in [0, 1_000_000, 12_340_000, 1_100_000, -2_500_000] {
            let cents = micro_usd_to_display(micro);
            let precise = micro_usd_to_display_precise(micro);
            assert_eq!(
                format!("{cents}00"),
                precise,
                "the two grains must not disagree at {micro} micro-USD"
            );
        }
    }

    #[test]
    fn extreme_values_neither_panic_nor_wrap() {
        // `unsigned_abs` is why i64::MIN needs no special case, and the
        // half-cent addend cannot overflow the u64 it is added to.
        assert_eq!(micro_usd_to_cents(i64::MAX), 922_337_203_685_478);
        assert_eq!(micro_usd_to_cents(i64::MIN), -922_337_203_685_478);
        // The finer grain divides by less, so it is the tighter cast.
        assert_eq!(
            round_to_units(i64::MAX, MICRO_PER_SUB_CENT),
            92_233_720_368_547_758
        );
        assert_eq!(
            round_to_units(i64::MIN, MICRO_PER_SUB_CENT),
            -92_233_720_368_547_758
        );
    }
}
