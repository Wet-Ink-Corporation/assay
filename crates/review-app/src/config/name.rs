//! Names of plugins and steps: safe as a path segment and as a report label.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

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

impl FromStr for GateName {
    type Err = ParseGateNameError;

    /// Reads a name written elsewhere (a job output) under the same rule as base config's.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text.to_owned()).map_err(|_refused| ParseGateNameError)
    }
}

/// Text that is not a plugin or step name: 1 to 64 of `a-z`, `0-9` and `-`, starting with a
/// letter or digit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("a plugin name is 1 to 64 of `a-z`, `0-9` and `-`, starting with a letter or digit")]
pub struct ParseGateNameError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_of_the_config_shape_parses_as_written() {
        let longest = "a".repeat(MAX_LEN);
        for text in ["temper", "0", "a-b-9", "x-", longest.as_str()] {
            assert_eq!(
                text.parse::<GateName>()
                    .map(|name| name.as_str().to_owned()),
                Ok(text.to_owned()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn any_other_text_is_not_a_name() {
        let too_long = "a".repeat(MAX_LEN + 1);
        for text in [
            "",
            "-a",
            "Temper",
            "a b",
            "a\nb",
            "a/b",
            "..",
            "a_b",
            "é",
            too_long.as_str(),
        ] {
            assert_eq!(
                text.parse::<GateName>(),
                Err(ParseGateNameError),
                "{text:?}"
            );
        }
    }
}
