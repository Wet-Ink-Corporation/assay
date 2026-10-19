//! Reading a record stream: newline-terminated lines of one JSON object each, parsed strictly.

use serde::de::DeserializeOwned;

use super::error::Corruption;
use super::port::MAX_RECORD_BYTES;
use crate::contract::parse_json_strict;

/// Where a file breaks a rule: the line, from 1, and the rule.
pub(super) type Refusal = (usize, Corruption);

/// The lines of `contents` with their numbers from 1, newlines removed. Empty contents hold no
/// line; otherwise every line must be non-empty, within [`MAX_RECORD_BYTES`], terminated, and
/// start with `{` and end with `}`: one object, with nothing around it, as the writer writes it.
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
        if !(line.starts_with(b"{") && line.ends_with(b"}")) {
            return Err((number, Corruption::Shape));
        }
        found.push((number, line));
    }
    Ok(found)
}

/// `text` as `D`: one JSON object with no repeated key, of `D`'s shape. Any other value is
/// refused, since serde would also read a struct from an array of its fields in order.
pub(super) fn parse<D: DeserializeOwned>(text: &[u8]) -> Result<D, Corruption> {
    let value = parse_json_strict(text).map_err(|_shape| Corruption::Shape)?;
    if !value.is_object() {
        return Err(Corruption::Shape);
    }
    serde_json::from_value(value).map_err(|_shape| Corruption::Shape)
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

/// Refuses `text` unless it is byte for byte what [`to_line`] writes for `wire`, the wire form
/// of the record read from it: assay's writer is the only writer, so any other form of the same
/// record (key order, spacing, escapes, number forms, a struct written as an array) was not
/// written by assay. `text` is a stream line without its newline, or a whole state document.
pub(super) fn check_canonical<S: serde::Serialize>(
    text: &[u8],
    wire: &S,
    newline: Newline,
) -> Result<(), Corruption> {
    let mut written = serde_json::to_vec(wire).map_err(|_shape| Corruption::Shape)?;
    if newline == Newline::Included {
        written.push(b'\n');
    }
    if text == written.as_slice() {
        Ok(())
    } else {
        Err(Corruption::NotCanonical)
    }
}

/// Whether the text [`check_canonical`] compares carries the writer's final newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Newline {
    /// A whole file, newline included.
    Included,
    /// A stream line, which [`lines`] gave without its newline.
    Stripped,
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
