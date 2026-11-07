//! A Weigh-In item id, checked by the rule decision 0003 records for `ItemId::parse`: `wi-`
//! then 4 to 16 lowercase hex characters. Unlike `ItemId::parse` nothing is trimmed: the
//! journal, the atoms and the trailer all write the bare id, so any whitespace is refused.

use std::fmt;

/// The prefix of every item id.
const PREFIX: &str = "wi-";
/// Fewest hex characters after the prefix.
const MIN_HEX: usize = 4;
/// Most hex characters after the prefix (an 8-character id follows a collision; 16 is the cap).
const MAX_HEX: usize = 16;

/// A validated item id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ItemId(String);

impl ItemId {
    /// `text` as an item id, or `None` when it is not exactly `wi-[0-9a-f]{4,16}`.
    pub(super) fn new(text: String) -> Option<Self> {
        let hex = text.strip_prefix(PREFIX)?;
        let valid = (MIN_HEX..=MAX_HEX).contains(&hex.len())
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        valid.then_some(Self(text))
    }

    /// The id's text.
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::ItemId;

    #[test]
    fn accepts_four_to_sixteen_lowercase_hex_characters() {
        for text in ["wi-0a1b", "wi-aa4c3b", "wi-0123abcd", "wi-0123456789abcdef"] {
            assert_eq!(
                ItemId::new(text.to_owned()).as_ref().map(ItemId::as_str),
                Some(text),
                "{text}"
            );
        }
    }

    #[test]
    fn refuses_anything_else() {
        let refused = [
            "",
            "wi-",
            "wi-abc",
            "wi-0123456789abcdef0",
            "wi-AA4C3B",
            "wi-aa4c3g",
            "WI-aa4c3b",
            "aa4c3b",
            " wi-aa4c3b",
            "wi-aa4c3b ",
            "wi-aa4c3b\n",
            "wi-aa4c\u{e9}",
        ];
        for text in refused {
            assert_eq!(ItemId::new(text.to_owned()), None, "{text:?}");
        }
    }
}
