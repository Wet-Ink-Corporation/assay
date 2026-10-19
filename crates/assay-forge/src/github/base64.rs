//! Strict standard base64 (RFC 4648 §4), as the contents API encodes files: line breaks are
//! skipped, padding is required, and anything else is refused. No base64 crate is
//! pre-approved, and this is the only place the adapter decodes it.

/// The standard alphabet, in value order.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The bytes `text` encodes, or `None` when it is not strict base64: whole groups of four,
/// `=` padding only in the last group, no non-zero bits dropped by the padding, and no
/// character outside the alphabet but `\n`, which is skipped.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let symbols: Vec<u8> = text.bytes().filter(|&byte| byte != b'\n').collect();
    let (groups, rest) = symbols.as_chunks::<4>();
    if !rest.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(groups.len().saturating_mul(3));
    let last = groups.len().checked_sub(1);
    for (index, &[a, b, c, d]) in groups.iter().enumerate() {
        let padding = match (c, d) {
            (b'=', b'=') => 2,
            (_, b'=') => 1,
            _ => 0,
        };
        if padding > 0 && Some(index) != last {
            return None;
        }
        let (a, b) = (value(a)?, value(b)?);
        let c = if padding == 2 { 0 } else { value(c)? };
        let d = if padding >= 1 { 0 } else { value(d)? };
        // The bits a padded group drops must be zero, so each text has one decoding.
        if (padding == 2 && b & 0x0f != 0) || (padding == 1 && c & 0x03 != 0) {
            return None;
        }
        let group = u32::from(a) << 18 | u32::from(b) << 12 | u32::from(c) << 6 | u32::from(d);
        let [_, first, second, third] = group.to_be_bytes();
        bytes.push(first);
        if padding < 2 {
            bytes.push(second);
        }
        if padding < 1 {
            bytes.push(third);
        }
    }
    Some(bytes)
}

/// The value of base64 symbol `symbol`.
fn value(symbol: u8) -> Option<u8> {
    let at = ALPHABET.iter().position(|&candidate| candidate == symbol)?;
    u8::try_from(at).ok()
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn padded_base64_across_lines_is_decoded() {
        assert_eq!(
            decode("aGVsbG8g\nd29ybGQ=\n").as_deref(),
            Some(&b"hello world"[..])
        );
        assert_eq!(decode("YQ==").as_deref(), Some(&b"a"[..]));
        assert_eq!(decode("YWI=").as_deref(), Some(&b"ab"[..]));
        assert_eq!(decode("+/+/").as_deref(), Some(&[0xfb, 0xff, 0xbf][..]));
        assert_eq!(decode("").as_deref(), Some(&b""[..]));
    }

    #[test]
    fn anything_but_strict_base64_is_refused() {
        for text in [
            "YQ", "YQ=", "Y===", "YQ==YQ==", "YQ\r\n==", "YW*=", "YWJj ", "-_8=", "YR==",
        ] {
            assert_eq!(decode(text), None, "{text:?}");
        }
    }
}
