//! Reading a record stream: newline-terminated lines of one JSON object each, parsed strictly.

use serde::de::DeserializeOwned;

use super::error::Corruption;
use super::port::MAX_RECORD_BYTES;
use crate::contract::parse_json_strict;

/// Where a file breaks a rule: the line, from 1, and the rule.
pub(super) type Refusal = (usize, Corruption);

/// The lines of `contents` with their numbers from 1, newlines removed. Empty contents hold no
/// line; otherwise every line must be non-empty, within [`MAX_RECORD_BYTES`], and terminated.
pub(super) fn lines(contents: &[u8]) -> Result<Vec<(usize, &[u8])>, Refusal> {
    let mut found = Vec::new();
    for (number, chunk) in (1_usize..).zip(contents.split_inclusive(|&byte| byte == b'\n')) {
        let terminated = chunk.strip_suffix(b"\n");
        let line = terminated.unwrap_or(chunk);
        if line.len() > MAX_RECORD_BYTES {
            return Err((number, Corruption::LongLine));
        }
        let Some(line) = terminated else {
            return Err((number, Corruption::Unterminated));
        };
        if line.is_empty() {
            return Err((number, Corruption::EmptyLine));
        }
        found.push((number, line));
    }
    Ok(found)
}

/// `text` as `D`: one JSON value with no repeated key, of `D`'s shape.
pub(super) fn parse<D: DeserializeOwned>(text: &[u8]) -> Result<D, Corruption> {
    parse_json_strict(text)
        .and_then(serde_json::from_value)
        .map_err(|_shape| Corruption::Shape)
}

/// Checks a record's `schema` field against the contract id it must carry.
pub(super) fn check_schema(found: &str, expected: &str) -> Result<(), Corruption> {
    if found == expected {
        Ok(())
    } else {
        Err(Corruption::Schema)
    }
}

/// `record` as one JSON line, newline included.
pub(super) fn to_line<S: serde::Serialize>(record: &S) -> Result<Vec<u8>, Corruption> {
    let mut line = serde_json::to_vec(record).map_err(|_shape| Corruption::Shape)?;
    line.push(b'\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_contents_hold_no_line() {
        assert_eq!(lines(b""), Ok(Vec::new()));
    }

    #[test]
    fn lines_are_numbered_from_one_without_their_newline() {
        assert_eq!(
            lines(b"{}\n{\"a\":1}\n"),
            Ok(vec![(1, b"{}".as_slice()), (2, b"{\"a\":1}".as_slice())])
        );
    }

    #[test]
    fn a_long_unterminated_tail_is_a_long_line() {
        let tail = vec![b'x'; MAX_RECORD_BYTES + 1];
        assert_eq!(lines(&tail), Err((1, Corruption::LongLine)));
    }
}
