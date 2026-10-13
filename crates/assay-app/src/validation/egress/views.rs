//! The ways the egress scan reads a text besides normalising it: as hex written one byte at a
//! time, as an escaped document read back, and character by character for lookalikes that
//! normalisation cannot map. Each is one linear pass.

use std::borrow::Cow;

use serde_json::Value;

use crate::contract::parse_json_strict;

/// What may stand before each byte of hex written one byte at a time, lowercase (after
/// folding): `\u00`, `u+00` (`U+0073`), `&#x`, `0x`, `\x` and `%`.
const BYTE_PREFIXES: [&[u8]; 6] = [b"\\u00", b"u+00", b"&#x", b"0x", b"\\x", b"%"];

/// The most decimal digits a character reference standing for one byte holds (`&#255;`).
const MAX_BYTE_DIGITS: usize = 3;

/// `text` as hex digits only: folded and lowercase, every [`BYTE_PREFIXES`] prefix deleted,
/// each decimal character reference of a byte (`&#115;`) read as that byte's two hex digits,
/// and every other character but `[0-9a-f]` dropped. `\x73\x6b`, `0x73, 0x6b`, `&#x73;`,
/// `%73`, `s`, `U+0073` and `&#115;` all read `736b…`.
///
/// Cost: linear; each step consumes at least one byte and tries a fixed set of prefixes.
pub(super) fn hex_view(text: &str) -> Vec<u8> {
    let folded = super::fold(text);
    let mut rest = folded.as_bytes();
    let mut view = Vec::with_capacity(rest.len());
    while let Some((&first, tail)) = rest.split_first() {
        if let Some(after) = BYTE_PREFIXES
            .iter()
            .find_map(|prefix| rest.strip_prefix(*prefix))
        {
            rest = after;
        } else if let Some((byte, after)) = decimal_reference(rest) {
            view.extend(super::encoding::hex(&[byte]).bytes());
            rest = after;
        } else {
            if first.is_ascii_hexdigit() {
                view.push(first);
            }
            rest = tail;
        }
    }
    view
}

/// The byte a decimal character reference at the start of `text` stands for (`&#` and one to
/// three digits of a value below 256; the `;` is optional, as browsers read it), and the text
/// after it.
fn decimal_reference(text: &[u8]) -> Option<(u8, &[u8])> {
    let digits = text.strip_prefix(b"&#")?;
    let count = digits
        .iter()
        .take(MAX_BYTE_DIGITS)
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let (number, after) = digits.split_at_checked(count)?;
    if number.is_empty() {
        return None;
    }
    let byte = number.iter().try_fold(0_u8, |value, digit| {
        value.checked_mul(10)?.checked_add(digit.checked_sub(b'0')?)
    })?;
    Some((byte, after.strip_prefix(b";").unwrap_or(after)))
}

/// Whether `c` is drawn like an ASCII letter or digit from a block that normalisation drops
/// rather than reads back: Mathematical Alphanumeric Symbols, Enclosed Alphanumerics and their
/// supplement, superscript and subscript letters, small capitals and modifier letters, a Latin
/// letter with no ASCII base, or any other non-ASCII numeric character. Such text is refused,
/// not mapped (wi-b5affa). Superscript and subscript digits are read as ASCII digits instead.
pub(super) fn is_obfuscated(c: char) -> bool {
    if super::latin::script_digit(c).is_some() {
        // Read as an ASCII digit (`O(n²)`), not refused.
        return false;
    }
    super::latin::is_unmapped_latin(c)
        || matches!(
            c,
            '\u{1d400}'..='\u{1d7ff}'
                | '\u{2460}'..='\u{24ff}'
                | '\u{1f100}'..='\u{1f1ff}'
                | '\u{2070}'..='\u{209f}'
                | '\u{1d00}'..='\u{1dbf}'
        )
        || (c.is_numeric() && !c.is_ascii_digit())
}

/// A document whose escapes could not be read back, so what it spells is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Ambiguous;

/// Runs `scan` over `document`'s texts until it finds something: its bytes as text, then every
/// key and string if it is strict JSON, else the whole text with its escapes read back (a
/// document the strict parse refuses, for a repeated key, a trailing byte or its depth, still
/// cannot hide an escaped token).
///
/// # Errors
///
/// [`Ambiguous`] when the strict parse fails and an escape in the text is not one of JSON's or
/// `\xHH`: the caller treats that as a hit.
pub(super) fn scan_views<R>(
    document: &[u8],
    mut scan: impl FnMut(&str) -> Option<R>,
) -> Result<Option<R>, Ambiguous> {
    let raw = String::from_utf8_lossy(document);
    if let Some(hit) = scan(&raw) {
        return Ok(Some(hit));
    }
    match parse_json_strict(document) {
        Ok(value) => Ok(json_strings(&value).into_iter().find_map(scan)),
        // Refused by the strict parse: read the escapes back without it.
        Err(_) => unescape(&raw).map(|text| scan(&text)).ok_or(Ambiguous),
    }
}

/// `text` with JSON's escapes (`\uXXXX`, surrogate pairs included, and the single-character
/// ones) and `\xHH` read back; `None` at any other backslash, a lone surrogate or a cut escape.
/// Text without a backslash is returned as is.
fn unescape(text: &str) -> Option<Cow<'_, str>> {
    if !text.contains('\\') {
        return Some(Cow::Borrowed(text));
    }
    let mut read = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            read.push(c);
            continue;
        }
        let escaped = match chars.next()? {
            '"' => '"',
            '\\' => '\\',
            '/' => '/',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'x' => char::from_u32(hex_digits(&mut chars, 2)?)?,
            'u' => unicode_escape(&mut chars)?,
            _ => return None,
        };
        read.push(escaped);
    }
    Some(Cow::Owned(read))
}

/// The character a `\u` escape spells, `\u` already read: one code unit, or a high surrogate
/// followed by `\u` and a low one.
fn unicode_escape(chars: &mut std::str::Chars<'_>) -> Option<char> {
    let unit = hex_digits(chars, 4)?;
    if !(0xd800..=0xdbff).contains(&unit) {
        // A lone low surrogate is no character: `from_u32` refuses it.
        return char::from_u32(unit);
    }
    if (chars.next()?, chars.next()?) != ('\\', 'u') {
        return None;
    }
    let low = hex_digits(chars, 4)?;
    if !(0xdc00..=0xdfff).contains(&low) {
        return None;
    }
    let high_bits = unit.checked_sub(0xd800)?.checked_shl(10)?;
    let low_bits = low.checked_sub(0xdc00)?;
    char::from_u32(0x1_0000_u32.checked_add(high_bits)?.checked_add(low_bits)?)
}

/// The value of the next `count` hex digits, or `None` if fewer follow.
fn hex_digits(chars: &mut std::str::Chars<'_>, count: usize) -> Option<u32> {
    (0..count).try_fold(0_u32, |value, _| {
        value
            .checked_mul(16)?
            .checked_add(chars.next()?.to_digit(16)?)
    })
}

/// Every object key and string in `value`, walked with an explicit stack.
fn json_strings(value: &Value) -> Vec<&str> {
    let mut strings = Vec::new();
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::String(text) => strings.push(text.as_str()),
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => {
                for (key, field) in fields {
                    strings.push(key.as_str());
                    pending.push(field);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    strings
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{hex_view, is_obfuscated, unescape};

    #[test]
    fn reads_every_byte_prefix_and_decimal_references_as_hex() {
        let texts = [
            "\\x73\\x6b",
            "0x73, 0x6b",
            "&#x73;&#x6B;",
            "%73%6B",
            "\\u0073\\u006b",
            "U+0073 U+006B",
            "&#115;&#107",
        ];

        assert_eq!(
            texts.map(hex_view),
            std::array::from_fn(|_| b"736b".to_vec())
        );
    }

    #[test]
    fn reads_json_escapes_and_surrogate_pairs_back() {
        assert_eq!(
            unescape(r#"a\"b\\c\/d\ns\x6b😀"#),
            Some(Cow::Owned("a\"b\\c/d\nsk\u{1f600}".to_owned()))
        );
    }

    #[test]
    fn refuses_escapes_it_cannot_read() {
        let texts = [
            r"\q", r"\ud83d", r"\ude00", r"\ud83dx", r"\u00", r"\x7", "end\\",
        ];

        assert_eq!(
            texts.map(unescape),
            [None, None, None, None, None, None, None]
        );
    }

    #[test]
    fn refuses_lookalikes_but_not_letters_of_other_scripts_or_raised_digits() {
        let refused = [
            '𝐀', '𝐳', '𝟗', '①', '🄰', 'ⁿ', 'ₐ', 'ᴀ', 'ᵃ', '٣', '５', '½', 'ß', 'ł',
        ];
        let read = ['a', 'é', 'ạ', 'ж', 'Ω', '中', '7', '²', '⁴', '₄'];

        assert_eq!(
            (refused.map(is_obfuscated), read.map(is_obfuscated)),
            ([true; 14], [false; 10])
        );
    }
}
