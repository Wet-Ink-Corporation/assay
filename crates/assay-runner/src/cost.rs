//! `total_cost_usd` (a JSON number of dollars) to [`Aeu`].

use std::num::IntErrorKind;

use assay_domain::Aeu;
use serde_json::Value;

/// Micro-AEU in one AEU.
const MICROS_PER_AEU: u64 = 1_000_000;

/// The cost a `total_cost_usd` value reports, rounded to the nearest micro-AEU, or `None` when it
/// is not a non-negative number. A cost past the largest `Aeu` saturates there, so an absurd
/// report is charged as much as possible rather than replaced by the smaller cap.
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
    let fraction: u64 = fraction.parse().ok()?;
    let micros = match whole.parse::<u64>() {
        Ok(whole) => whole
            .saturating_mul(MICROS_PER_AEU)
            .saturating_add(fraction),
        // The text is digits only, so the parse fails only on overflow.
        Err(error) if *error.kind() == IntErrorKind::PosOverflow => u64::MAX,
        Err(_) => return None,
    };
    Some(Aeu::from_micros(micros))
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
    fn saturates_costs_too_large_to_hold() {
        let most = Some(Aeu::from_micros(u64::MAX));

        assert_eq!(from_usd(&json!(1e30)), most);
        assert_eq!(from_usd(&json!(18_446_744_073_709.6)), most);
    }

    #[test]
    fn rejects_values_that_are_not_numbers() {
        assert_eq!(from_usd(&json!("0.42")), None);
    }
}
