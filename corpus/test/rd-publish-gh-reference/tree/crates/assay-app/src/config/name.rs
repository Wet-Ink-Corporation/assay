//! Names of plugins and steps: safe as a path segment and as a report label.

use std::fmt;

/// The longest accepted name, in bytes.
const MAX_LEN: usize = 64;

/// A plugin or step name: 1 to 64 of `a-z`, `0-9` and `-`, starting with a letter or digit.
/// Adapters use it as a directory name, so it can never hold `/` or `..`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GateName(String);

impl GateName {
    /// Accepts `name` when it has the documented shape, handing it back otherwise.
    pub(crate) fn new(name: String) -> Result<Self, String> {
        let valid = name.len() <= MAX_LEN
            && name
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid { Ok(Self(name)) } else { Err(name) }
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
