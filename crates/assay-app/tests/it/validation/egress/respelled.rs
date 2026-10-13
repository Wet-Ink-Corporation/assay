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
fn flags_the_token_in_mathematical_letters() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        kind_of(&respelled(mathematical_bold))?,
        Some(EgressKind::Obfuscated)
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
fn flags_a_lookalike_alphanumeric_from_each_refused_block() -> Result<(), Box<dyn Error>> {
    // Mathematical, circled, parenthesised-squared, superscript, small capital, non-ASCII digit.
    let lookalikes = ["𝐳", "①", "🄰", "⁴", "²", "ᴀ", "٣"];
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
