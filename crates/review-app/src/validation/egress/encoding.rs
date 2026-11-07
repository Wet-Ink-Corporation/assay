//! The encodings of the token the egress scan looks for: base64 and hex, written out here
//! rather than taken from a crate, since only the encoding direction is needed.

/// The standard base64 alphabet (RFC 4648 §4). The URL-safe one differs only in `+` and `/`,
/// which normalisation strips, so one alphabet covers both.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Lowercase hex digits.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// The base64 of `bytes`' whole three-byte groups: the characters that depend on `bytes` alone
/// when they start a group. A trailing partial group is left out, since its last character
/// depends on whatever follows.
pub(super) fn base64_whole_groups(bytes: &[u8]) -> String {
    let (groups, _partial) = bytes.as_chunks::<3>();
    let mut encoded = String::with_capacity(groups.len().saturating_mul(4));
    for &[a, b, c] in groups {
        let sextets = [
            a >> 2,
            (a & 0b11) << 4 | b >> 4,
            (b & 0b1111) << 2 | c >> 6,
            c & 0b11_1111,
        ];
        // Each sextet is below 64, so every lookup succeeds.
        encoded.extend(
            sextets
                .iter()
                .filter_map(|&sextet| BASE64.get(usize::from(sextet)))
                .map(|&symbol| char::from(symbol)),
        );
    }
    encoded
}

/// `bytes` in lowercase hex.
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|&byte| [byte >> 4, byte & 0b1111])
        // Each nibble is below 16, so every lookup succeeds.
        .filter_map(|nibble| HEX.get(usize::from(nibble)))
        .map(|&digit| char::from(digit))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{base64_whole_groups, hex};

    #[test]
    fn encodes_whole_groups_as_rfc_4648_does() {
        // RFC 4648 §10 test vectors whose length is a multiple of three.
        assert_eq!(
            ["", "foo", "foobar"].map(|text| base64_whole_groups(text.as_bytes())),
            ["", "Zm9v", "Zm9vYmFy"]
        );
    }

    #[test]
    fn leaves_out_a_trailing_partial_group() {
        assert_eq!(
            ["fo", "foob", "fooba"].map(|text| base64_whole_groups(text.as_bytes())),
            ["", "Zm9v", "Zm9v"]
        );
    }

    #[test]
    fn uses_both_symbols_past_the_alphanumerics() {
        assert_eq!(base64_whole_groups(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn writes_lowercase_hex() {
        assert_eq!(hex(&[0x00, 0x7e, 0xab, 0xff]), "007eabff");
    }
}
