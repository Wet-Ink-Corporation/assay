//! Just enough of Rust's lexical grammar to read attributes out of a PR's source: identifiers,
//! lifetimes, punctuation and literals with their lines, with comments dropped.
//!
//! The source is PR-controlled, so the token boundaries follow `rustc_lexer` (rustc 1.98):
//! whole lifetimes and labels (`'r#x` raw), literal prefixes (`b'`, `b"`, `br`, `c"`, `cr`,
//! `r"`, `r#"`) and suffixes (`"x"r`, `'a'r`, `1u8`), rustc's number shape, raw identifiers
//! (the word after `r#` is a plain identifier), and a leading shebang line. A literal or comment
//! that never ends is an error the caller fails on; anything else it does not recognise is
//! punctuation. `corpus.rs` checks the boundaries against `proc-macro2`'s lexer.

use std::num::NonZeroU64;

/// One token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Token<'a> {
    /// An identifier or keyword; `raw` for `r#name`, whose `name` is without the `r#`.
    Ident { name: &'a str, raw: bool },
    /// A lifetime or label: its text after the quote (`a`, `r#a`).
    Lifetime(&'a str),
    /// One punctuation character; `::` is two `:`.
    Punct(char),
    /// A string, byte, C string, raw string, character or number literal, as written, with its
    /// prefix and suffix.
    Literal(&'a str),
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
    let mut lexer = Lexer::new(strip_shebang(text));
    while let Some(next) = lexer.rest.chars().next() {
        lexer.step(next)?;
    }
    Ok(lexer.out)
}

/// `text` without its shebang line, as `rustc_lexer::strip_shebang` removes it: a first line
/// starting `#!` whose next token (past whitespace and comments) is not `[`. The line break
/// stays, so lines still count from the file's first. When the rest does not lex, nothing is
/// stripped: reading the line as code can only show more.
fn strip_shebang(text: &str) -> &str {
    let Some(tail) = text.strip_prefix("#!") else {
        return text;
    };
    let mut lexer = Lexer::new(tail);
    while lexer.out.is_empty() {
        let Some(next) = lexer.rest.chars().next() else {
            break;
        };
        if lexer.step(next).is_err() {
            return text;
        }
    }
    match lexer.out.first() {
        Some(first) if first.token == Token::Punct('[') => text,
        _ => text
            .find('\n')
            .and_then(|end| text.get(end..))
            .unwrap_or(""),
    }
}

struct Lexer<'a> {
    rest: &'a str,
    line: NonZeroU64,
    out: Vec<Lexed<'a>>,
}

impl<'a> Lexer<'a> {
    const fn new(text: &'a str) -> Self {
        Self {
            rest: text,
            line: NonZeroU64::MIN,
            out: Vec::new(),
        }
    }

    /// Consumes the token or the comment that starts with `next`.
    fn step(&mut self, next: char) -> Result<(), Unterminated> {
        let (start, line) = (self.rest, self.line);
        let ended = match next {
            '/' if self.rest.starts_with("//") => {
                let end = self.rest.find('\n').unwrap_or(self.rest.len());
                self.skip(end)
            }
            '/' if self.rest.starts_with("/*") => self.block_comment(),
            '"' => self
                .bump()
                .and_then(|()| self.quoted('"'))
                .map(|()| self.literal(start, line)),
            '\'' => self.char_or_lifetime(),
            next if next.is_ascii_digit() => self.number().map(|()| self.literal(start, line)),
            next if is_ident_start(next) => self.word(),
            next if next.is_whitespace() => self.bump(),
            next => self.bump().map(|()| self.push(Token::Punct(next), line)),
        };
        ended.ok_or(Unterminated(line))
    }

    fn push(&mut self, token: Token<'a>, line: NonZeroU64) {
        self.out.push(Lexed { token, line });
    }

    /// Eats a literal suffix and records the literal that started at `start` on `line`.
    fn literal(&mut self, start: &'a str, line: NonZeroU64) {
        self.eat_identifier();
        let consumed = start.len().saturating_sub(self.rest.len());
        let text = start.get(..consumed).unwrap_or(start);
        self.push(Token::Literal(text), line);
    }

    fn first(&self) -> Option<char> {
        self.rest.chars().next()
    }

    fn nth(&self, n: usize) -> Option<char> {
        self.rest.chars().nth(n)
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

    /// Consumes the characters `keep` accepts, and returns them.
    fn eat_while(&mut self, keep: impl Fn(char) -> bool) -> &'a str {
        let end = self
            .rest
            .find(|c: char| !keep(c))
            .unwrap_or(self.rest.len());
        let (eaten, rest) = self.rest.split_at_checked(end).unwrap_or((self.rest, ""));
        self.rest = rest;
        eaten
    }

    /// Consumes an identifier if one starts here (a literal's suffix), and returns it.
    fn eat_identifier(&mut self) -> &'a str {
        if self.first().is_some_and(is_ident_start) {
            self.eat_while(is_ident_continue)
        } else {
            ""
        }
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

    /// Consumes the rest of a literal up to the unescaped `close`.
    fn quoted(&mut self, close: char) -> Option<()> {
        loop {
            let next = self.first()?;
            self.bump()?;
            if next == '\\' {
                self.bump()?;
            } else if next == close {
                return Some(());
            }
        }
    }

    /// The rest of a character or byte literal after its quote, as `rustc_lexer`'s
    /// `single_quoted_string` ends it; `None` when it does not end.
    fn single_quoted(&mut self) -> Option<()> {
        if self.nth(1) == Some('\'') && self.first() != Some('\\') {
            self.bump()?;
            return self.bump();
        }
        loop {
            match (self.first()?, self.nth(1)) {
                ('\'', _) => return self.bump(),
                ('/', _) => return None,
                ('\n', second) if second != Some('\'') => return None,
                ('\\', _) => {
                    self.bump()?;
                    self.bump()?;
                }
                _ => self.bump()?,
            }
        }
    }

    /// A character literal (`'x'`, `'\n'`, `'"'r`) or a whole lifetime or label (`'a`,
    /// `'r#a`), as `rustc_lexer`'s `lifetime_or_char` tells them apart.
    fn char_or_lifetime(&mut self) -> Option<()> {
        let (start, line) = (self.rest, self.line);
        self.bump()?;
        let lifetime = self.nth(1) != Some('\'')
            && self
                .first()
                .is_some_and(|c| is_ident_start(c) || c.is_ascii_digit());
        if !lifetime {
            self.single_quoted()?;
            self.literal(start, line);
            return Some(());
        }
        let name_start = self.rest;
        if self.first() == Some('r')
            && self.nth(1) == Some('#')
            && self.nth(2).is_some_and(is_ident_start)
        {
            self.skip(2)?;
        }
        self.bump()?;
        self.eat_while(is_ident_continue);
        if self.first() == Some('\'') {
            // `'ab'`: rustc reads a (malformed) character literal.
            self.bump()?;
            let consumed = start.len().saturating_sub(self.rest.len());
            self.push(Token::Literal(start.get(..consumed).unwrap_or(start)), line);
            return Some(());
        }
        let length = name_start.len().saturating_sub(self.rest.len());
        let name = name_start.get(..length).unwrap_or(name_start);
        self.push(Token::Lifetime(name), line);
        Some(())
    }

    /// A number literal without its suffix, as `rustc_lexer`'s `number` ends it.
    fn number(&mut self) -> Option<()> {
        let zero = self.first() == Some('0');
        self.bump()?;
        let digits = |c: char| c.is_ascii_digit() || c == '_';
        if zero {
            match self.first() {
                Some('b' | 'o') => {
                    self.bump()?;
                    self.eat_while(digits);
                    return Some(());
                }
                Some('x') => {
                    self.bump()?;
                    self.eat_while(|c| c.is_ascii_hexdigit() || c == '_');
                    return Some(());
                }
                Some('0'..='9' | '_') => {
                    self.eat_while(digits);
                }
                Some('.' | 'e' | 'E') => {}
                _ => return Some(()),
            }
        } else {
            self.eat_while(digits);
        }
        match (self.first(), self.nth(1)) {
            (Some('.'), second) if second != Some('.') && !second.is_some_and(is_ident_start) => {
                self.bump()?;
                if self.first().is_some_and(|c| c.is_ascii_digit()) {
                    self.eat_while(digits);
                    if matches!(self.first(), Some('e' | 'E')) {
                        self.exponent()?;
                    }
                }
            }
            (Some('e' | 'E'), _) => self.exponent()?,
            _ => {}
        }
        Some(())
    }

    /// An exponent from its `e`: an optional sign, then digits.
    fn exponent(&mut self) -> Option<()> {
        self.bump()?;
        if matches!(self.first(), Some('-' | '+')) {
            self.bump()?;
        }
        self.eat_while(|c| c.is_ascii_digit() || c == '_');
        Some(())
    }

    /// An identifier, a raw identifier, or the literal a `b`, `c`, `r`, `br` or `cr` prefix
    /// starts, as `rustc_lexer`'s `advance_token` tells them apart.
    fn word(&mut self) -> Option<()> {
        let (start, line) = (self.rest, self.line);
        match (self.first(), self.nth(1), self.nth(2)) {
            (Some('r'), Some('#'), Some(third)) if is_ident_start(third) => {
                self.skip(2)?;
                let name = self.eat_while(is_ident_continue);
                self.push(Token::Ident { name, raw: true }, line);
                Some(())
            }
            (Some('r'), Some('#' | '"'), _) => {
                self.bump()?;
                self.raw_string(start, line)
            }
            (Some('b'), Some('\''), _) => {
                self.skip(2)?;
                self.single_quoted()?;
                self.literal(start, line);
                Some(())
            }
            (Some('b' | 'c'), Some('"'), _) => {
                self.skip(2)?;
                self.quoted('"')?;
                self.literal(start, line);
                Some(())
            }
            (Some('b' | 'c'), Some('r'), Some('"' | '#')) => {
                self.skip(2)?;
                self.raw_string(start, line)
            }
            _ => {
                let name = self.eat_while(is_ident_continue);
                self.push(Token::Ident { name, raw: false }, line);
                Some(())
            }
        }
    }

    /// The rest of a raw string from its `#`s: `"`, then anything up to `"` and as many `#`s,
    /// then a suffix. Anything else after the `#`s is a malformed raw string, refused.
    fn raw_string(&mut self, start: &'a str, line: NonZeroU64) -> Option<()> {
        let hashes = self.eat_while(|c| c == '#').len();
        if self.first() != Some('"') {
            return None;
        }
        self.bump()?;
        let close = format!("\"{}", "#".repeat(hashes));
        let end = self.rest.find(&close)?.checked_add(close.len())?;
        self.skip(end)?;
        self.literal(start, line);
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

    /// The tokens of `text` as written.
    fn texts(text: &str) -> Vec<String> {
        tokens(text)
            .expect("terminated")
            .into_iter()
            .map(|lexed| match lexed.token {
                Token::Ident { name, raw: false } => name.to_owned(),
                Token::Ident { name, raw: true } => format!("r#{name}"),
                Token::Lifetime(name) => format!("'{name}"),
                Token::Punct(c) => c.to_string(),
                Token::Literal(text) => text.to_owned(),
            })
            .collect()
    }

    #[test]
    fn comments_are_dropped_and_literals_kept_whole() {
        let text = "a /* x /* y */ z */ b // c\n\"d\\\" e\" r#\"f\"g\"# br##\"h\"## '\"' 'l";

        assert_eq!(
            texts(text),
            [
                "a",
                "b",
                "\"d\\\" e\"",
                "r#\"f\"g\"#",
                "br##\"h\"##",
                "'\"'",
                "'l"
            ],
            "nested comments, escapes, raw strings and a lifetime"
        );
    }

    #[test]
    fn prefixes_suffixes_and_raw_names_end_where_rustc_ends_them() {
        let text =
            "'r\"x\" \"x\"r \"y\" 'a'r b'a'r r#r\"z\" 'br 'r#a 1.5e-3f64 0x1Fu8 1..2 x.0 1.max";

        assert_eq!(
            texts(text),
            [
                "'r",
                "\"x\"",
                "\"x\"r",
                "\"y\"",
                "'a'r",
                "b'a'r",
                "r#r",
                "\"z\"",
                "'br",
                "'r#a",
                "1.5e-3f64",
                "0x1Fu8",
                "1",
                ".",
                ".",
                "2",
                "x",
                ".",
                "0",
                "1",
                ".",
                "max"
            ],
            "each token as rustc_lexer splits it"
        );
    }

    #[test]
    fn a_shebang_line_is_skipped_unless_an_inner_attribute_follows() {
        assert_eq!(
            (
                texts("#!/usr/bin/env run \"\nfn f() {}"),
                texts("#! /* c */ [allow(x)]")
            ),
            (
                vec![
                    "fn".to_owned(),
                    "f".to_owned(),
                    "(".to_owned(),
                    ")".to_owned(),
                    "{".to_owned(),
                    "}".to_owned()
                ],
                ["#", "!", "[", "allow", "(", "x", ")", "]"]
                    .map(str::to_owned)
                    .to_vec()
            ),
            "rustc_lexer::strip_shebang"
        );
    }

    #[test]
    fn tokens_carry_the_line_they_start_on() {
        let lexed = tokens("#!shebang\na\n\"x\ny\"\nb").expect("terminated");

        let lines: Vec<u64> = lexed.iter().map(|lexed| lexed.line.get()).collect();
        assert_eq!(
            lines,
            [2, 3, 5],
            "the shebang keeps its line; a multi-line literal starts on 3"
        );
    }

    #[test]
    fn an_unterminated_literal_or_comment_is_an_error() {
        for text in [
            "a\n\"open",
            "a\n/* open",
            "a\nr#\"open\"",
            "a\n'\\",
            "a\nr##x",
        ] {
            assert_eq!(
                tokens(text),
                Err(Unterminated(NonZeroU64::MIN.saturating_add(1))),
                "{text}"
            );
        }
    }
}
