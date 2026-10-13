//! The token respelled so normalisation alone cannot see it: in lookalike alphanumerics, and in
//! hex written one byte at a time behind a prefix.

use std::error::Error;

use assay_app::EgressKind;
use serde_json::json;

use super::super::fixture::finding_with;
use super::{TOKEN, kind_of, z4};

/// `text` with a space after every 15 characters, so no run reaches 20 and only the token
/// windows or the lookalike rule can catch it.
fn spaced(text: &str) -> String {
    text.chars()
        .enumerate()
        .flat_map(|(at, c)| {
            let space = (at > 0 && at % 15 == 0).then_some(' ');
            space.into_iter().chain([c])
        })
        .collect()
}

/// `TOKEN` with each ASCII letter and digit replaced by `respell`'s character.
fn respelled(respell: impl Fn(char) -> Option<char>) -> String {
    spaced(
        &TOKEN
            .chars()
            .map(|c| respell(c).unwrap_or(c))
            .collect::<String>(),
    )
}

/// The mathematical bold form of an ASCII letter or digit (U+1D400, U+1D41A, U+1D7CE).
fn mathematical_bold(c: char) -> Option<char> {
    let (base, start) = match c {
        'A'..='Z' => (0x1d400, 'A'),
        'a'..='z' => (0x1d41a, 'a'),
        '0'..='9' => (0x1d7ce, '0'),
        _ => return None,
    };
    char::from_u32(base + (u32::from(c) - u32::from(start)))
}

/// The fullwidth form of an ASCII digit (U+FF10).
fn fullwidth_digit(c: char) -> Option<char> {
    c.is_ascii_digit()
        .then(|| char::from_u32(0xff10 + (u32::from(c) - u32::from('0'))))
        .flatten()
}

#[test]
fn reads_the_token_in_mathematical_letters() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        kind_of(&respelled(mathematical_bold))?,
        Some(EgressKind::Token)
    );
    Ok(())
}

#[test]
fn reads_fullwidth_digits_as_ascii_ones() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        kind_of(&respelled(fullwidth_digit))?,
        Some(EgressKind::Token)
    );
    Ok(())
}

#[test]
fn flags_letter_like_symbols_and_numerics_no_table_reads() -> Result<(), Box<dyn Error>> {
    // A fraction, a circled twenty, a Roman numeral, a mathematical Greek capital: numeric or
    // letter-like symbols no table reads as one ASCII letter or digit.
    let lookalikes = ["\u{bd}", "\u{2473}", "\u{216b}", "\u{1d6aa}"];
    let findings: Vec<_> = lookalikes
        .iter()
        .map(|c| finding_with(json!({ "body": format!("see {c} here") })))
        .collect();

    let validated = z4(&findings)?;

    let kinds: Vec<_> = validated.egress.iter().map(|hit| hit.kind).collect();
    assert_eq!(kinds, vec![EgressKind::Obfuscated; lookalikes.len()]);
    Ok(())
}

/// `TOKEN`'s bytes as two hex digits each, behind `prefix` and followed by `suffix`.
fn per_byte(prefix: &str, suffix: &str) -> String {
    TOKEN
        .bytes()
        .map(|byte| format!("{prefix}{byte:02x}{suffix}"))
        .collect::<Vec<_>>()
        .concat()
}

/// The egress kind of a body holding `TOKEN` written one byte at a time as `prefix`, hex,
/// `suffix`.
fn per_byte_kind(prefix: &str, suffix: &str) -> Result<Option<EgressKind>, Box<dyn Error>> {
    kind_of(&per_byte(prefix, suffix))
}

#[test]
fn catches_hex_bytes_behind_backslash_x() -> Result<(), Box<dyn Error>> {
    assert_eq!(per_byte_kind("\\x", "")?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn catches_hex_bytes_behind_0x_in_a_list() -> Result<(), Box<dyn Error>> {
    assert_eq!(per_byte_kind("0x", ", ")?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn catches_hex_character_references() -> Result<(), Box<dyn Error>> {
    assert_eq!(per_byte_kind("&#x", ";")?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn catches_unicode_escapes() -> Result<(), Box<dyn Error>> {
    assert_eq!(per_byte_kind("\\u00", "")?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn catches_code_points() -> Result<(), Box<dyn Error>> {
    assert_eq!(per_byte_kind("U+00", " ")?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn catches_percent_encoding() -> Result<(), Box<dyn Error>> {
    assert_eq!(per_byte_kind("%", "")?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn catches_decimal_character_references() -> Result<(), Box<dyn Error>> {
    let decimal = TOKEN
        .bytes()
        .map(|byte| format!("&#{byte};"))
        .collect::<Vec<_>>()
        .concat();

    assert_eq!(kind_of(&decimal)?, Some(EgressKind::Token));
    Ok(())
}

/// `TOKEN` with each ASCII letter that has one written with a dot below (or above), case kept,
/// spaced every 15 characters (generated with Python's `unicodedata.normalize("NFC", c + mark)`).
const DOTTED: &str = "\u{1e63}\u{1e33}-\u{1ea1}\u{1e47}\u{1e6d}-\u{1ecd}\u{1ea1}\u{1e6d}01-\u{1e92}q 3\u{1e36}\u{1e89}9_\u{1e8a}\u{1e7f}2\u{1e5a}\u{1e6d}~~~\u{1e24}\u{1e57} 8\u{1e46}\u{10b}\u{1ef4}\u{1e05}4?\u{1e42}\u{1e0d}\u{1e32}\u{1e63}7\u{1ee4}\u{1eb9}1 J\u{1ea1}6\u{120}\u{1e1f}0\u{1e6c}\u{1ecb}5\u{1e88}\u{1ecd}>>?";

#[test]
fn reads_the_token_in_dotted_latin_letters() -> Result<(), Box<dyn Error>> {
    assert_eq!(kind_of(DOTTED)?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn reads_the_token_with_superscript_digits() -> Result<(), Box<dyn Error>> {
    // Every digit raised: no twelve ASCII characters of the token are left in a row.
    let raised = respelled(|c| match c {
        '0' => Some('\u{2070}'),
        '1' => Some('\u{b9}'),
        '2' => Some('\u{b2}'),
        '3' => Some('\u{b3}'),
        '4'..='9' => char::from_u32(0x2070 + (u32::from(c) - u32::from('0'))),
        _ => None,
    });

    assert_eq!(kind_of(&raised)?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn passes_superscript_and_subscript_digits_in_prose() -> Result<(), Box<dyn Error>> {
    let prose =
        finding_with(json!({ "body": "The loop is O(n\u{b2}) over x\u{2081} to x\u{2089}." }));

    let validated = z4(&[prose])?;

    assert_eq!(validated.egress, Vec::new());
    Ok(())
}

/// `TOKEN` with each ASCII capital replaced by the letter of `script` at the same offset from
/// `first` (Lisu U+A4D0, Cherokee U+13A0), lowercase letters and digits kept, spaced every 15.
fn capitals_from(first: u32) -> String {
    respelled(|c| {
        c.is_ascii_uppercase()
            .then(|| char::from_u32(first + (u32::from(c) - u32::from('A'))))
            .flatten()
    })
}

#[test]
fn reads_the_token_with_lisu_capitals() -> Result<(), Box<dyn Error>> {
    assert_eq!(kind_of(&capitals_from(0xa4d0))?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn reads_the_token_with_cherokee_capitals() -> Result<(), Box<dyn Error>> {
    assert_eq!(kind_of(&capitals_from(0x13a0))?, Some(EgressKind::Token));
    Ok(())
}

#[test]
fn passes_german_nordic_and_polish_prose() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        kind_of("Die Funktion hei\u{df}t gro\u{df}; see S\u{f8}ren and \u{141}ukasz.")?,
        None
    );
    Ok(())
}

#[test]
fn passes_a_cjk_paragraph() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        kind_of(
            "\u{8fd9}\u{4e2a}\u{51fd}\u{6570}\u{5728}\u{8f93}\u{5165}\u{4e3a}\u{7a7a}\u{65f6}\u{4f1a}\u{5d29}\u{6e83}\u{ff0c}\u{8bf7}\u{5148}\u{68c0}\u{67e5}\u{957f}\u{5ea6}\u{3002}"
        )?,
        None
    );
    Ok(())
}

#[test]
fn needs_seven_ascii_symbols_in_a_window_holding_lookalikes() -> Result<(), Box<dyn Error>> {
    // The token's first twelve normalised symbols are `skantoat01zq`; `\u{4e2d}` stands for any
    // character no table reads as ASCII.
    let seven = "s\u{4e2d}a\u{4e2d}t\u{4e2d}a\u{4e2d}0\u{4e2d}zq";
    let six = "s\u{4e2d}a\u{4e2d}t\u{4e2d}a\u{4e2d}0\u{4e2d}z\u{4e2d}";

    assert_eq!(
        (kind_of(seven)?, kind_of(six)?),
        (Some(EgressKind::Token), None)
    );
    Ok(())
}

/// Each ASCII letter's stroked or barred Latin form (none for `m`, `w` and `x`).
fn stroked(c: char) -> Option<char> {
    let lower = "\u{2c65}\u{180}\u{23c}\u{111}\u{247}\u{a799}\u{1e5}\u{127}\u{268}\u{249}\u{a741}\u{142}m\u{a7a5}\u{f8}\u{a751}\u{a757}\u{24d}\u{a7a9}\u{167}\u{289}\u{a75f}wx\u{24f}\u{1b6}";
    let index =
        usize::try_from(u32::from(c.to_ascii_lowercase()).checked_sub(u32::from('a'))?).ok()?;
    let form = lower.chars().nth(index).filter(|form| !form.is_ascii())?;
    if c.is_ascii_uppercase() {
        form.to_uppercase().next()
    } else {
        Some(form)
    }
}

#[test]
fn reads_the_token_in_stroked_latin_letters() -> Result<(), Box<dyn Error>> {
    assert_eq!(kind_of(&respelled(stroked))?, Some(EgressKind::Token));
    Ok(())
}
