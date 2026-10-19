//! AEU as the ledger writes it: a decimal string in dollars, exactly as [`Aeu`] displays it
//! (`"0.74"`, `"2"`, `"0.000001"`), so a sum over records is exact (no `f64`).

use assay_domain::Aeu;

/// Micro-AEU in one AEU.
const MICROS_PER_AEU: u64 = 1_000_000;

/// Decimal digits of a micro-AEU fraction.
const FRACTION_DIGITS: usize = 6;

/// `text` as an amount when it is exactly how [`Aeu`] displays one: whole dollars without a
/// leading zero (but `0`), then optionally `.` and one to six digits without a trailing zero.
pub(super) fn parse_aeu(text: &str) -> Option<Aeu> {
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };
    let canonical_whole = whole == "0" || !whole.starts_with('0');
    if !canonical_whole {
        return None;
    }
    let whole = digits(whole)?;
    let fraction = match fraction {
        None => 0,
        Some(fraction) if fraction.len() <= FRACTION_DIGITS && !fraction.ends_with('0') => {
            let scale = FRACTION_DIGITS.checked_sub(fraction.len())?;
            let factor = 10_u64.checked_pow(u32::try_from(scale).ok()?)?;
            digits(fraction)?.checked_mul(factor)?
        }
        Some(_) => return None,
    };
    let micros = whole.checked_mul(MICROS_PER_AEU)?.checked_add(fraction)?;
    Some(Aeu::from_micros(micros))
}

/// The value of `text` when it is one or more ASCII digits and fits a `u64`.
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_displayed_forms() {
        assert_eq!(parse_aeu("0.74"), Some(Aeu::from_micros(740_000)));
        assert_eq!(parse_aeu("2"), Some(Aeu::from_micros(2_000_000)));
        assert_eq!(parse_aeu("0.000001"), Some(Aeu::from_micros(1)));
        assert_eq!(parse_aeu("0"), Some(Aeu::ZERO));
    }

    #[test]
    fn refuses_any_other_form() {
        for text in [
            "",
            "0.740",
            "00.74",
            "01",
            ".5",
            "5.",
            "-1",
            "+1",
            "1e3",
            "0.0000001",
            " 1",
            "1.0",
            "0x10",
        ] {
            assert_eq!(parse_aeu(text), None, "{text:?}");
        }
    }

    #[test]
    fn refuses_an_amount_past_the_largest_one() {
        assert_eq!(
            parse_aeu("18446744073709.551615"),
            Some(Aeu::from_micros(u64::MAX))
        );
        assert_eq!(parse_aeu("18446744073709.551616"), None);
    }

    proptest::proptest! {
        #[test]
        fn every_amount_reads_back_from_its_display(micros: u64) {
            let amount = Aeu::from_micros(micros);
            proptest::prop_assert_eq!(parse_aeu(&amount.to_string()), Some(amount));
        }
    }
}
