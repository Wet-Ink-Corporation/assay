//! The small glob dialect of the attribute allowlist.
//!
//! `*` matches any run of characters except `/`, `?` one character except `/`, `**` anything,
//! and `**/` zero or more whole directories. A glob without `/` matches the file name at any
//! depth (as in `.gitattributes`); one with `/` matches the whole path from the root.
//! Character classes, braces, negation and escapes are rejected rather than half-supported.
//! Matching is a linear-space dynamic programme, so no glob can make it exponential.

use std::str::FromStr;

/// Why a string is not a supported allowlist glob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GlobError {
    /// The glob is empty.
    #[error("an allowlist glob must not be empty")]
    Empty,
    /// The glob starts with `/`; globs are already relative to the repository root.
    #[error("an allowlist glob must not start with `/`")]
    Anchored,
    /// The glob uses syntax this dialect does not support.
    #[error("an allowlist glob must not use `{0}`")]
    Unsupported(char),
}

/// One element of a parsed glob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// This exact character.
    Literal(char),
    /// `?`: one character other than `/`.
    One,
    /// `*`: any run of characters without `/`.
    Star,
    /// `**` not followed by `/`: any run of characters.
    AnyDepth,
    /// `**/`: nothing, or any run of characters ending in `/`.
    AnyDirs,
}

/// A parsed allowlist glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Glob {
    tokens: Vec<Token>,
    /// Matched against the file name only (the glob has no `/`).
    name_only: bool,
}

impl FromStr for Glob {
    type Err = GlobError;

    fn from_str(glob: &str) -> Result<Self, Self::Err> {
        if glob.is_empty() {
            return Err(GlobError::Empty);
        }
        if glob.starts_with('/') {
            return Err(GlobError::Anchored);
        }
        let mut tokens = Vec::new();
        let mut chars = glob.chars().peekable();
        while let Some(c) = chars.next() {
            let token = match c {
                '[' | ']' | '{' | '}' | '!' | '\\' => return Err(GlobError::Unsupported(c)),
                '?' => Token::One,
                '*' if chars.next_if_eq(&'*').is_some() => {
                    if chars.next_if_eq(&'/').is_some() {
                        Token::AnyDirs
                    } else {
                        Token::AnyDepth
                    }
                }
                '*' => Token::Star,
                literal => Token::Literal(literal),
            };
            tokens.push(token);
        }
        Ok(Self {
            tokens,
            name_only: !glob.contains('/'),
        })
    }
}

impl Glob {
    /// Whether `path`, a `/`-separated git path, matches.
    pub(super) fn matches(&self, path: &str) -> bool {
        let subject = if self.name_only {
            path.rsplit_once('/').map_or(path, |(_, name)| name)
        } else {
            path
        };
        let text: Vec<char> = subject.chars().collect();
        // reachable[j]: the tokens so far can match exactly text[..j].
        let mut reachable = vec![false; text.len().saturating_add(1)];
        if let Some(start) = reachable.first_mut() {
            *start = true;
        }
        for &token in &self.tokens {
            reachable = step(token, &reachable, &text);
        }
        reachable.last().copied().unwrap_or(false)
    }
}

/// Extends the set of reachable text positions by one token.
fn step(token: Token, reachable: &[bool], text: &[char]) -> Vec<bool> {
    let mut next = vec![false; reachable.len()];
    match token {
        Token::Literal(_) | Token::One => {
            for ((slot, &from), &c) in next.iter_mut().skip(1).zip(reachable).zip(text) {
                *slot = from
                    && match token {
                        Token::Literal(expected) => c == expected,
                        _ => c != '/',
                    };
            }
        }
        Token::Star | Token::AnyDepth => {
            // carry: some reachable start with no `/` (for `*`) between it and here.
            let mut carry = false;
            let following = text.iter().map(Some).chain([None]);
            for ((slot, &from), c) in next.iter_mut().zip(reachable).zip(following) {
                carry |= from;
                *slot = carry;
                if token == Token::Star && c == Some(&'/') {
                    carry = false;
                }
            }
        }
        Token::AnyDirs => {
            // seen: some position before this one is reachable.
            let mut seen = false;
            let preceding = [None].into_iter().chain(text.iter().map(Some));
            for ((slot, &from), previous) in next.iter_mut().zip(reachable).zip(preceding) {
                *slot = from || (seen && previous == Some(&'/'));
                seen |= from;
            }
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(glob: &str, path: &str) -> bool {
        glob.parse::<Glob>().expect("a valid glob").matches(path)
    }

    #[test]
    fn a_glob_without_a_slash_matches_the_file_name_at_any_depth() {
        assert!(matches("*.png", "logo.png"), "top level");
        assert!(matches("*.png", "assets/img/logo.png"), "nested");
        assert!(
            !matches("*.png", "logo.png.rs"),
            "the whole name must match"
        );
    }

    #[test]
    fn a_glob_with_a_slash_matches_the_whole_path() {
        assert!(matches("assets/*.png", "assets/logo.png"), "one level");
        assert!(
            !matches("assets/*.png", "assets/img/logo.png"),
            "`*` does not cross a slash"
        );
        assert!(
            !matches("assets/*.png", "x/assets/logo.png"),
            "anchored at the root"
        );
    }

    #[test]
    fn double_star_crosses_directories() {
        assert!(matches("assets/**", "assets/img/logo.png"), "trailing **");
        assert!(
            matches("**/*.png", "logo.png"),
            "**/ matches zero directories"
        );
        assert!(matches("**/*.png", "a/b/logo.png"), "**/ matches several");
        assert!(matches("a/**/b.bin", "a/b.bin"), "inner **/ may be empty");
        assert!(
            matches("a/**/b.bin", "a/x/y/b.bin"),
            "inner **/ spans levels"
        );
    }

    #[test]
    fn question_mark_matches_one_character_but_not_a_slash() {
        assert!(matches("v?.bin", "v1.bin"), "one character");
        assert!(!matches("v?.bin", "v12.bin"), "exactly one");
        assert!(!matches("a?b", "a/b"), "never a slash");
    }

    #[test]
    fn rejects_empty_anchored_and_unsupported_globs() {
        for (glob, expected) in [
            ("", GlobError::Empty),
            ("/assets/*.png", GlobError::Anchored),
            ("*.[ch]", GlobError::Unsupported('[')),
            ("*.{png,jpg}", GlobError::Unsupported('{')),
            ("!*.png", GlobError::Unsupported('!')),
            ("a\\*", GlobError::Unsupported('\\')),
        ] {
            assert_eq!(glob.parse::<Glob>(), Err(expected), "{glob:?}");
        }
    }
}
