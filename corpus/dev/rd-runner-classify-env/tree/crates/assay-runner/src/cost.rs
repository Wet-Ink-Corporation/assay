//! `total_cost_usd` (a JSON number of dollars) to [`Aeu`].

use assay_domain::Aeu;
use serde_json::Value;

/// Micro-AEU in one AEU.
const MICROS_PER_AEU: u64 = 1_000_000;

/// The cost a `total_cost_usd` value reports, rounded to the nearest micro-AEU, or `None` when it
/// is not a non-negative number that fits.
///
/// The float is rendered with six fixed decimals and parsed back as integers, which converts it
/// without a lossy `as` cast (H-11); fixed-precision formatting never uses an exponent.
pub(crate) fn from_usd(value: &Value) -> Option<Aeu> {
    let dollars = value.as_f64()?;
    if dollars.is_sign_negative() {
        return None;
    }
    let text = format!("{dollars:.6}");
    let (whole, fraction) = text.split_once('.')?;
    let whole: u64 = whole.parse().ok()?;
    let fraction: u64 = fraction.parse().ok()?;
    whole
        .checked_mul(MICROS_PER_AEU)?
        .checked_add(fraction)
        .map(Aeu::from_micros)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Aeu, from_usd};

    #[test]
    fn converts_a_fraction_of_a_dollar() {
        assert_eq!(from_usd(&json!(0.4213)), Some(Aeu::from_micros(421_300)));
    }

    #[test]
    fn converts_whole_dollars_given_as_an_integer() {
        assert_eq!(from_usd(&json!(2)), Some(Aeu::from_micros(2_000_000)));
    }

    #[test]
    fn rounds_to_the_nearest_micro_aeu() {
        assert_eq!(from_usd(&json!(0.000_000_6)), Some(Aeu::from_micros(1)));
        assert_eq!(from_usd(&json!(1e-9)), Some(Aeu::ZERO));
    }

    #[test]
    fn rejects_negative_costs() {
        assert_eq!(from_usd(&json!(-0.5)), None);
    }

    #[test]
    fn rejects_costs_too_large_to_hold() {
        assert_eq!(from_usd(&json!(1e30)), None);
    }

    #[test]
    fn rejects_values_that_are_not_numbers() {
        assert_eq!(from_usd(&json!("0.42")), None);
    }
}
