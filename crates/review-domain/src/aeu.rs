//! AEU: API-equivalent dollars, the CLI's client-side cost estimate used as a relative measure of
//! consumption (`docs/architecture/06-operations.md` §1).

use std::fmt;

/// Micro-AEU in one AEU.
const MICROS_PER_AEU: u64 = 1_000_000;

/// Decimal digits of a micro-AEU fraction.
const FRACTION_DIGITS: usize = 6;

/// An amount of AEU (API-equivalent dollars), held exactly as a whole number of micro-AEU.
///
/// Caps (`--max-budget-usd`) and the real cost of a model call are both `Aeu`, so a cap can be
/// charged as a cost without conversion. It displays as a plain decimal in dollars (`0.74`, `2`,
/// `0.000001`), the form the CLI's `--max-budget-usd` flag reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Aeu {
    micros: u64,
}

impl Aeu {
    /// Nothing spent.
    pub const ZERO: Self = Self { micros: 0 };

    /// The amount of `micros` millionths of an AEU.
    #[must_use]
    pub const fn from_micros(micros: u64) -> Self {
        Self { micros }
    }

    /// This amount in millionths of an AEU.
    #[must_use]
    pub const fn micros(self) -> u64 {
        self.micros
    }

    /// The sum of two amounts, saturating at the largest representable amount (about 1.8e13
    /// AEU), so a total never wraps to a small charge.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self {
            micros: self.micros.saturating_add(other.micros),
        }
    }
}

impl fmt::Display for Aeu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.micros / MICROS_PER_AEU;
        let mut fraction = self.micros % MICROS_PER_AEU;
        if fraction == 0 {
            return write!(f, "{whole}");
        }
        // Drop trailing zeros: the digits left keep their leading zeros through the width. A
        // non-zero fraction below 10^6 has at most five trailing zeros, so `width` stays >= 1.
        let mut width = FRACTION_DIGITS;
        while fraction.is_multiple_of(10) {
            fraction /= 10;
            width -= 1;
        }
        write!(f, "{whole}.{fraction:0width$}")
    }
}

#[cfg(test)]
mod tests {
    use super::Aeu;

    #[test]
    fn displays_whole_dollars_without_a_fraction() {
        assert_eq!(Aeu::from_micros(2_000_000).to_string(), "2");
    }

    #[test]
    fn displays_a_fraction_without_trailing_zeros() {
        assert_eq!(Aeu::from_micros(740_000).to_string(), "0.74");
    }

    #[test]
    fn displays_one_micro_aeu_with_its_leading_zeros() {
        assert_eq!(Aeu::from_micros(1).to_string(), "0.000001");
    }

    #[test]
    fn displays_zero_as_zero() {
        assert_eq!(Aeu::ZERO.to_string(), "0");
    }

    #[test]
    fn saturating_add_sums_and_stops_at_the_maximum() {
        assert_eq!(
            Aeu::from_micros(1).saturating_add(Aeu::from_micros(2)),
            Aeu::from_micros(3)
        );
        assert_eq!(
            Aeu::from_micros(u64::MAX).saturating_add(Aeu::from_micros(1)),
            Aeu::from_micros(u64::MAX)
        );
    }
}
