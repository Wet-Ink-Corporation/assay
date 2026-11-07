//! A read document, as a tree, and the lookups the workflow facts make on it.

/// A read document, as a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    /// No value (`key:` with nothing under it), or an empty document.
    Null,
    /// A plain, single- or double-quoted scalar, unquoted.
    Scalar(String),
    /// A block scalar (`|` or `>`), its lines joined with `\n` without their indentation.
    Text(String),
    /// A sequence.
    Seq(Vec<Node>),
    /// A mapping, in file order, with no key twice.
    Map(Vec<(String, Node)>),
}

impl Node {
    /// The value of `key` when this is a mapping holding it.
    pub(crate) fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value),
            Self::Null | Self::Scalar(_) | Self::Text(_) | Self::Seq(_) => None,
        }
    }

    /// Takes the value of `key` out of this mapping, when it holds it.
    pub(crate) fn remove(&mut self, key: &str) -> Option<Self> {
        match self {
            Self::Map(entries) => {
                let at = entries.iter().position(|(candidate, _)| candidate == key)?;
                Some(entries.remove(at).1)
            }
            Self::Null | Self::Scalar(_) | Self::Text(_) | Self::Seq(_) => None,
        }
    }

    /// Whether `needle` appears in any key or scalar under this node, in any ASCII case (GitHub
    /// reads expression contexts such as `secrets` in any case).
    pub(crate) fn mentions_ignoring_case(&self, needle: &str) -> bool {
        match self {
            Self::Null => false,
            Self::Scalar(text) | Self::Text(text) => contains_ignoring_case(text, needle),
            Self::Seq(items) => items.iter().any(|item| item.mentions_ignoring_case(needle)),
            Self::Map(entries) => entries.iter().any(|(key, value)| {
                contains_ignoring_case(key, needle) || value.mentions_ignoring_case(needle)
            }),
        }
    }
}

/// Whether `haystack` holds `needle` in any ASCII case.
fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    // `windows` needs a non-zero width; every text holds the empty needle.
    needle.is_empty()
        || haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}
