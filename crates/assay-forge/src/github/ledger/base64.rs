//! Base64 (RFC 4648 §4, standard alphabet, padded) for the contents API, which sends and takes
//! file contents in it. No base64 crate is pre-approved, and the codec is small.

/// The standard alphabet, by sextet value.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The padding character.
const PAD: u8 = b'=';

/// `bytes` in padded standard base64, without line breaks.
pub(super) fn encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let (group, sextets) = match *chunk {
            [a] => (u32::from(a) << 16, 2),
            [a, b] => ((u32::from(a) << 16) | (u32::from(b) << 8), 3),
            [a, b, c] => ((u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(c), 4),
            _ => (0, 0),
        };
        for index in 0..4_u32 {
            let shift = 18 - 6 * index;
            let symbol = if index < sextets {
                usize::try_from((group >> shift) & 0x3f)
                    .ok()
                    .and_then(|sextet| ALPHABET.get(sextet))
                    .copied()
                    .unwrap_or(PAD)
            } else {
                PAD
            };
            encoded.push(char::from(symbol));
        }
    }
    encoded
}

/// The bytes `text` encodes, when it is padded standard base64 in its canonical form (unused
/// bits zero), with any number of `\n` between characters, as GitHub wraps content at 60
/// columns. Anything else is `None`.
pub(super) fn decode(text: &str) -> Option<Vec<u8>> {
    let symbols: Vec<u8> = text.bytes().filter(|&byte| byte != b'\n').collect();
    let (quads, rest) = symbols.as_chunks::<4>();
    if !rest.is_empty() {
        return None;
    }
    let mut decoded = Vec::with_capacity(quads.len().saturating_mul(3));
    let last = quads.len().checked_sub(1);
    for (index, quad) in quads.iter().enumerate() {
        let padding = match quad {
            [_, _, PAD, PAD] => 2,
            [_, _, _, PAD] => 1,
            _ => 0,
        };
        if padding > 0 && Some(index) != last {
            return None;
        }
        let mut group = 0_u32;
        for &symbol in quad.iter().take(4 - padding) {
            group = (group << 6) | sextet(symbol)?;
        }
        group <<= 6 * padding;
        let [_, a, b, c] = group.to_be_bytes();
        match padding {
            0 => decoded.extend_from_slice(&[a, b, c]),
            1 if c == 0 => decoded.extend_from_slice(&[a, b]),
            2 if b == 0 && c == 0 => decoded.push(a),
            _ => return None,
        }
    }
    Some(decoded)
}

/// The value of one alphabet symbol; padding and anything else is `None`.
fn sextet(symbol: u8) -> Option<u32> {
    let value = match symbol {
        b'A'..=b'Z' => symbol - b'A',
        b'a'..=b'z' => symbol - b'a' + 26,
        b'0'..=b'9' => symbol - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 §10.
    const VECTORS: [(&str, &str); 7] = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn encodes_the_rfc_vectors() {
        for (plain, encoded) in VECTORS {
            assert_eq!(encode(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn decodes_the_rfc_vectors() {
        for (plain, encoded) in VECTORS {
            assert_eq!(
                decode(encoded),
                Some(plain.as_bytes().to_vec()),
                "{encoded:?}"
            );
        }
    }

    #[test]
    fn decodes_github_s_line_wrapped_content() {
        assert_eq!(decode("Zm9v\nYmFy\n"), Some(b"foobar".to_vec()));
    }

    #[test]
    fn refuses_anything_but_padded_standard_base64() {
        for text in [
            "Zm9",
            "Zg",
            "Zg=",
            "Zg===",
            "Zm9v-_",
            "Zm9v YmFy",
            "Zm=v",
            "Zg==Zg==",
            "Zh==",
            "Zm9=",
            "\r\nZm9v",
        ] {
            assert_eq!(decode(text), None, "{text:?}");
        }
    }

    proptest::proptest! {
        #[test]
        fn decoding_an_encoding_gives_the_bytes_back(bytes: Vec<u8>) {
            proptest::prop_assert_eq!(decode(&encode(&bytes)), Some(bytes));
        }

        #[test]
        fn an_encoding_uses_only_the_alphabet_and_padding(bytes: Vec<u8>) {
            let encoded = encode(&bytes);
            proptest::prop_assert!(encoded.len().is_multiple_of(4));
            proptest::prop_assert!(encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')));
        }
    }
}
