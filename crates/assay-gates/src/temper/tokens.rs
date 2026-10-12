//! Just enough of Rust's lexical grammar to read attributes out of a PR's source: identifiers,
//! punctuation and literals with their lines, with comments dropped and the insides of string
//! and character literals never mistaken for code.
//!
//! The source is PR-controlled, so where this lexer and rustc could disagree it errs towards
//! seeing more code: an unterminated literal or comment is an error the caller fails on, and
//! anything it does not recognise is punctuation.

use std::num::NonZeroU64;

/// One token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Token<'a> {
    /// An identifier (a raw identifier `r#x` without its `r#`), or a keyword.
    Ident(&'a str),
    /// One punctuation character; `::` is two `:`.
    Punct(char),
    /// A string, byte string, raw string, character or number literal.
    Literal,
}

/// A token and the 1-based line it starts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Lexed<'a> {
    pub(super) token: Token<'a>,
    pub(super) line: NonZeroU64,
}

/// A literal or block comment that starts on this line and never ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unterminated(pub(super) NonZeroU64);

/// The tokens of `text`, in order.
pub(super) fn tokens(text: &str) -> Result<Vec<Lexed<'_>>, Unterminated> {
    let mut lexer = Lexer {
        rest: text,
        line: NonZeroU64::MIN,
        out: Vec::new(),
    };
    while let Some(next) = lexer.rest.chars().next() {
        lexer.step(next)?;
    }
    Ok(lexer.out)
}

struct Lexer<'a> {
    rest: &'a str,
    line: NonZeroU64,
    out: Vec<Lexed<'a>>,
}

impl<'a> Lexer<'a> {
    /// Consumes the token or the comment that starts with `next`.
    fn step(&mut self, next: char) -> Result<(), Unterminated> {
        let start = self.line;
        let ended = match next {
            '/' if self.rest.starts_with("//") => {
                let end = self.rest.find('\n').unwrap_or(self.rest.len());
                self.skip(end)
            }
            '/' if self.rest.starts_with("/*") => self.block_comment(),
            '"' => self.bump().and_then(|()| self.quoted('"')),
            '\'' => self.char_or_lifetime(),
            next if next.is_ascii_digit() => {
                let end = self.rest.find(|c: char| !is_ident_continue(c));
                self.skip(end.unwrap_or(self.rest.len()))
                    .map(|()| self.push(Token::Literal, start))
            }
            next if is_ident_start(next) => self.ident_or_raw(),
            next if next.is_whitespace() => self.bump(),
            next => self.bump().map(|()| self.push(Token::Punct(next), start)),
        };
        ended.ok_or(Unterminated(start))
    }

    fn push(&mut self, token: Token<'a>, line: NonZeroU64) {
        self.out.push(Lexed { token, line });
    }

    /// Consumes one character.
    fn bump(&mut self) -> Option<()> {
        let mut chars = self.rest.chars();
        let next = chars.next()?;
        if next == '\n' {
            self.line = self.line.saturating_add(1);
        }
        self.rest = chars.as_str();
        Some(())
    }

    /// Consumes the next `bytes` bytes, counting the lines they end.
    fn skip(&mut self, bytes: usize) -> Option<()> {
        let (skipped, rest) = self.rest.split_at_checked(bytes)?;
        let lines = u64::try_from(skipped.matches('\n').count()).ok()?;
        self.line = self.line.saturating_add(lines);
        self.rest = rest;
        Some(())
    }

    /// Consumes a block comment, nested ones included, from its `/*`.
    fn block_comment(&mut self) -> Option<()> {
        let mut depth = 0_usize;
        loop {
            if self.rest.starts_with("/*") {
                depth = depth.checked_add(1)?;
                self.skip(2)?;
            } else if self.rest.starts_with("*/") {
                depth = depth.checked_sub(1)?;
                self.skip(2)?;
                if depth == 0 {
                    return Some(());
                }
            } else {
                self.bump()?;
            }
        }
    }

    /// Consumes the rest of a literal up to the unescaped `close`, and records it.
    fn quoted(&mut self, close: char) -> Option<()> {
        let start = self.line;
        loop {
            let next = self.rest.chars().next()?;
            self.bump()?;
            if next == '\\' {
                self.bump()?;
            } else if next == close {
                self.push(Token::Literal, start);
                return Some(());
            }
        }
    }

    /// A character literal (`'x'`, `'\n'`, `'"'`) or the quote of a lifetime or label.
    fn char_or_lifetime(&mut self) -> Option<()> {
        let start = self.line;
        self.bump()?;
        let mut ahead = self.rest.chars();
        match (ahead.next(), ahead.next()) {
            (Some('\\'), _) => self.quoted('\''),
            (Some(_), Some('\'')) => {
                self.bump()?;
                self.bump()?;
                self.push(Token::Literal, start);
                Some(())
            }
            _ => {
                self.push(Token::Punct('\''), start);
                Some(())
            }
        }
    }

    /// An identifier, a raw identifier, or the raw string an `r`, `br` or `cr` prefix starts.
    fn ident_or_raw(&mut self) -> Option<()> {
        let start = self.line;
        let end = self
            .rest
            .find(|c: char| !is_ident_continue(c))
            .unwrap_or(self.rest.len());
        let (word, rest) = self.rest.split_at_checked(end)?;
        self.rest = rest;
        if !matches!(word, "r" | "br" | "cr") {
            self.push(Token::Ident(word), start);
            return Some(());
        }
        let hashes = rest
            .len()
            .saturating_sub(rest.trim_start_matches('#').len());
        let after = rest.trim_start_matches('#');
        if after.starts_with('"') {
            return self.raw_string(hashes);
        }
        match (word, hashes, after.chars().next()) {
            ("r", 1, Some(next)) if is_ident_start(next) => {
                self.skip(1)?;
                self.ident_or_raw()
            }
            _ => {
                self.push(Token::Ident(word), start);
                Some(())
            }
        }
    }

    /// Consumes a raw string from its `#`s: `"`, then anything up to `"` and `hashes` `#`s.
    fn raw_string(&mut self, hashes: usize) -> Option<()> {
        let start = self.line;
        self.skip(hashes.checked_add(1)?)?;
        let close = format!("\"{}", "#".repeat(hashes));
        let end = self.rest.find(&close)?.checked_add(close.len())?;
        self.skip(end)?;
        self.push(Token::Literal, start);
        Some(())
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idents(text: &str) -> Vec<&str> {
        tokens(text)
            .expect("terminated")
            .into_iter()
            .filter_map(|lexed| match lexed.token {
                Token::Ident(word) => Some(word),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn literals_and_comments_hide_no_code_and_show_none() {
        let text = "a /* x /* y */ z */ b // c\n\"d\\\" e\" r#\"f\"g\"# br##\"h\"## \
                    '\"' i 'j' '\\'' k 'l m r#n";

        assert_eq!(
            idents(text),
            ["a", "b", "i", "k", "l", "m", "n"],
            "comments and literals are skipped whole; a lifetime's name and a raw identifier are kept"
        );
    }

    #[test]
    fn tokens_carry_the_line_they_start_on() {
        let lexed = tokens("a\n\"x\ny\"\nb").expect("terminated");

        let lines: Vec<u64> = lexed.iter().map(|lexed| lexed.line.get()).collect();
        assert_eq!(lines, [1, 2, 4], "a multi-line literal starts on line 2");
    }

    #[test]
    fn an_unterminated_literal_or_comment_is_an_error() {
        for text in ["a\n\"open", "a\n/* open", "a\nr#\"open\"", "a\n'\\"] {
            assert_eq!(
                tokens(text),
                Err(Unterminated(NonZeroU64::MIN.saturating_add(1))),
                "{text}"
            );
        }
    }
}
