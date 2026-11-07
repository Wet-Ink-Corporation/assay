use super::Proportion;

fn upper(hits: u32, n: u32) -> f64 {
    Proportion { hits, n }
        .wilson_upper()
        .expect("n is positive")
}

/// Published 95% Wilson upper bounds (z = 1.96), to four decimals.
#[test]
fn the_wilson_upper_bound_of_none_in_thirty_is_0_1135() {
    assert!((upper(0, 30) - 0.1135).abs() < 5e-5, "{}", upper(0, 30));
}

#[test]
fn the_wilson_upper_bound_of_three_in_thirty_is_0_2562() {
    assert!((upper(3, 30) - 0.2562).abs() < 5e-5, "{}", upper(3, 30));
}

#[test]
fn the_wilson_upper_bound_of_all_is_one() {
    assert!((upper(30, 30) - 1.0).abs() < 1e-12, "{}", upper(30, 30));
}

#[test]
fn a_proportion_of_nothing_has_no_rate_and_no_bound() {
    let empty = Proportion { hits: 0, n: 0 };

    assert_eq!((empty.rate(), empty.wilson_upper()), (None, None));
}
