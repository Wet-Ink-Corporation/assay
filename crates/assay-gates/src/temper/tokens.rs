//! Just enough of Rust's lexical grammar to read attributes out of a PR's source: identifiers,
//! lifetimes, punctuation and literals with their lines, with comments dropped.
//!
//! The source is PR-controlled, so the token boundaries follow `rustc_lexer` (rustc 1.98):
//! whole lifetimes and labels (`'r#x` raw), literal prefixes (`b'`, `b"`, `br`, `c"`, `r"`,
//! `r#"`) and suffixes (`"x"r`, `'a'r`, `1u8`), rustc's number shape, raw identifiers (the word
//! after `r#` is a plain identifier), and a leading byte order mark. `corpus.rs` checks the
//! boundaries against `proc-macro2`'s lexer.
//!
//! Boundaries follow edition 2021 and later. The forms whose boundaries differ in an earlier
//! edition are refused, since a crate on edition 2018 would read the code after them
//! differently: a `cr"` or `cr#` C string, a raw lifetime followed by a quote (`'r#a'`), and a
//! literal suffix, lifetime or raw identifier ending in `r` that is followed at once by `#` or
//! `"` (`'r"x"`, `"x"r"y"`, `r#r"z"`), where a `r`, `br` or `cr` prefix may start a literal.
//! A shebang line is refused too: which lines `rustc_lexer::strip_shebang` drops is not
//! modelled here, so the file is not read at all.
//!
//! Code must be ASCII. rustc reads identifiers, lifetimes and some whitespace by Unicode tables
//! (`XID_Start`, `Pattern_White_Space`) this lexer does not carry, so any other character
//! outside a literal or comment is an error the caller fails on, as is a literal or comment
//! that never ends and a leading frontmatter block. Literal and comment boundaries are found by
//! ASCII rules only. Any other ASCII character is punctuation.

use std::borrow::Cow;
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

/// Where this lexer stops reading a file, at the line it happens on: a literal or block
/// comment that never ends, a character outside ASCII in code, or a leading frontmatter block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unlexable(pub(super) NonZeroU64);

/// A `.rs` file's text as rustc lexes it: one leading byte order mark removed and every CRLF
/// made LF, as rustc normalises a source file before lexing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Source<'a>(Cow<'a, str>);

impl<'a> Source<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        Self(if text.contains("\r\n") {
            Cow::Owned(text.replace("\r\n", "\n"))
        } else {
            Cow::Borrowed(text)
        })
    }

    /// The normalised text.
    pub(super) fn text(&self) -> &str {
        &self.0
    }

    /// The tokens of the text, in order. A shebang line (one `rustc_lexer::strip_shebang`
    /// would drop) is refused at line 1, and frontmatter (`---` first, past whitespace) at its
    /// line, since rustc's lexer does not tokenise either.
    pub(super) fn tokens(&self) -> Result<Vec<Lexed<'_>>, Unlexable> {
        let body = &*self.0;
        if has_shebang(body) {
            return Err(Unlexable(NonZeroU64::MIN));
        }
        let mut lexer = Lexer::new(body);
        let lead = body.trim_start_matches(is_pattern_white_space);
        if lead.starts_with("---") {
            lexer
                .skip(body.len().saturating_sub(lead.len()))
                .ok_or(Unlexable(NonZeroU64::MIN))?;
            return Err(Unlexable(lexer.line));
        }
        while let Some(next) = lexer.first() {
            lexer.step(next)?;
        }
        Ok(lexer.out)
    }
}

/// Whether `text` starts with a line `rustc_lexer::strip_shebang` would drop: `#!` whose next
/// token, past whitespace and comments other than doc comments, is not `[`.
fn has_shebang(text: &str) -> bool {
    let Some(tail) = text.strip_prefix("#!") else {
        return false;
    };
    let mut lexer = Lexer::new(tail);
    while lexer.skip_trivia().is_some() {}
    lexer.first() != Some('[')
}

/// rustc's whitespace: Unicode's `Pattern_White_Space`.
const fn is_pattern_white_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0B}'
            | '\u{0C}'
            | '\r'
            | ' '
            | '\u{85}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Whether `rest` starts a doc line comment: `///` (not `////`) or `//!`.
fn is_doc_line(rest: &str) -> bool {
    (rest.starts_with("///") && !rest.starts_with("////")) || rest.starts_with("//!")
}

/// Whether `rest` starts a doc block comment: `/*!`, or `/**` (not `/***` or `/**/`).
fn is_doc_block(rest: &str) -> bool {
    rest.starts_with("/*!")
        || (rest.starts_with("/**") && !rest.starts_with("/***") && !rest.starts_with("/**/"))
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

    /// Consumes one whitespace character or one comment that is not a doc comment; `None` when
    /// neither starts here, or a block comment runs to the end.
    fn skip_trivia(&mut self) -> Option<()> {
        let rest = self.rest;
        match self.first()? {
            c if is_pattern_white_space(c) => self.bump(),
            '/' if rest.starts_with("//") && !is_doc_line(rest) => {
                self.skip(rest.find('\n').unwrap_or(rest.len()))
            }
            '/' if rest.starts_with("/*") && !is_doc_block(rest) => self.block_comment(),
            _ => None,
        }
    }

    /// Consumes the token or the comment that starts with `next`.
    fn step(&mut self, next: char) -> Result<(), Unlexable> {
        let (start, line) = (self.rest, self.line);
        let ended = match next {
            next if !next.is_ascii() => None,
            '/' if self.rest.starts_with("//") => {
                let end = self.rest.find('\n').unwrap_or(self.rest.len());
                self.skip(end)
            }
            '/' if self.rest.starts_with("/*") => self.block_comment(),
            '"' => self
                .bump()
                .and_then(|()| self.quoted('"'))
                .and_then(|()| self.literal(start, line)),
            '\'' => self.char_or_lifetime(),
            next if next.is_ascii_digit() => self.number().and_then(|()| self.literal(start, line)),
            next if is_ident_start(next) => self.word(),
            next if is_pattern_white_space(next) => self.bump(),
            next => self.bump().map(|()| self.push(Token::Punct(next), line)),
        };
        ended.ok_or(Unlexable(line))
    }

    fn push(&mut self, token: Token<'a>, line: NonZeroU64) {
        self.out.push(Lexed { token, line });
    }

    /// Eats a literal suffix and records the literal that started at `start` on `line`; `None`
    /// when a suffix ending in `r` runs into a `#` or `"` (see [`Self::prefix_follows`]).
    fn literal(&mut self, start: &'a str, line: NonZeroU64) -> Option<()> {
        let suffix = self.eat_identifier();
        if self.prefix_follows(suffix) {
            return None;
        }
        let consumed = start.len().saturating_sub(self.rest.len());
        let text = start.get(..consumed).unwrap_or(start);
        self.push(Token::Literal(text), line);
        Some(())
    }

    /// Whether `name`, a literal suffix, lifetime or raw identifier just consumed, ends in `r`
    /// and runs into a `#` or `"`: whether that `r`, `br` or `cr` starts a raw literal there
    /// differs between editions and lexers (edition 2021 reserves such prefixes), and this
    /// lexer does not know the edition, so the file is refused.
    fn prefix_follows(&self, name: &str) -> bool {
        name.ends_with('r') && matches!(self.first(), Some('#' | '"'))
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
    /// `'r#a`), as `rustc_lexer`'s `lifetime_or_char` tells them apart. A quote then a
    /// character outside ASCII not closed at once (`'℘ `) is refused: whether rustc reads a
    /// lifetime there depends on Unicode tables.
    fn char_or_lifetime(&mut self) -> Option<()> {
        let (start, line) = (self.rest, self.line);
        self.bump()?;
        if self.first().is_some_and(|c| !c.is_ascii()) && self.nth(1) != Some('\'') {
            return None;
        }
        let lifetime = self.nth(1) != Some('\'')
            && self
                .first()
                .is_some_and(|c| is_ident_start(c) || c.is_ascii_digit());
        if !lifetime {
            self.single_quoted()?;
            return self.literal(start, line);
        }
        let name_start = self.rest;
        let raw = self.first() == Some('r')
            && self.nth(1) == Some('#')
            && self.nth(2).is_some_and(is_ident_start);
        if raw {
            self.skip(2)?;
        }
        self.bump()?;
        self.eat_while(is_ident_continue);
        if self.first() == Some('\'') {
            // `'r#a'`: before edition 2021, `'r` then `#` then `a'..`, which ends elsewhere.
            if raw {
                return None;
            }
            // `'ab'`: rustc reads a (malformed) character literal.
            self.bump()?;
            let consumed = start.len().saturating_sub(self.rest.len());
            self.push(Token::Literal(start.get(..consumed).unwrap_or(start)), line);
            return Some(());
        }
        let length = name_start.len().saturating_sub(self.rest.len());
        let name = name_start.get(..length).unwrap_or(name_start);
        if self.prefix_follows(name) {
            return None;
        }
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

    /// An identifier, a raw identifier, or the literal a `b`, `c`, `r` or `br` prefix starts,
    /// as `rustc_lexer`'s `advance_token` tells them apart; a `cr` prefix is refused.
    fn word(&mut self) -> Option<()> {
        let (start, line) = (self.rest, self.line);
        match (self.first(), self.nth(1), self.nth(2)) {
            (Some('r'), Some('#'), Some(third)) if is_ident_start(third) => {
                self.skip(2)?;
                let name = self.eat_while(is_ident_continue);
                if self.prefix_follows(name) {
                    return None;
                }
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
                self.literal(start, line)
            }
            (Some('b' | 'c'), Some('"'), _) => {
                self.skip(2)?;
                self.quoted('"')?;
                self.literal(start, line)
            }
            // Before edition 2021, `cr` is an identifier and the string after it ends elsewhere.
            (Some('c'), Some('r'), Some('"' | '#')) => None,
            (Some('b'), Some('r'), Some('"' | '#')) => {
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
        self.literal(start, line)
    }
}

/// An ASCII identifier's first character; others are refused before this is asked.
const fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

/// An ASCII identifier's later character.
const fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens of `text` as written.
    fn texts(text: &str) -> Vec<String> {
        Source::new(text)
            .tokens()
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
            "'r \"x\" \"x\"r \"y\" 'a'r b'a'r r#r \"z\" 'br 'r#a 1.5e-3f64 0x1Fu8 1..2 x.0 1.max";

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
    fn a_name_ending_in_r_against_a_quote_or_hash_is_an_error_at_its_line() {
        for text in [
            "a\n'r\"x\"",
            "a\n'br#x",
            "a\n\"x\"r\"y\"",
            "a\n1r\"x\"",
            "a\nb'a'cr#x",
            "a\nr#r\"z\"",
            "a\nr#\"x\"#r#",
        ] {
            assert_eq!(
                Source::new(text).tokens(),
                Err(Unlexable(NonZeroU64::MIN.saturating_add(1))),
                "{text}"
            );
        }
    }

    #[test]
    fn a_shebang_line_is_an_error_at_line_1_unless_an_inner_attribute_follows() {
        assert_eq!(
            (
                Source::new("#!/usr/bin/env run \"\nfn f() {}").tokens(),
                texts("#! /* c */ [allow(x)]")
            ),
            (
                Err(Unlexable(NonZeroU64::MIN)),
                ["#", "!", "[", "allow", "(", "x", ")", "]"]
                    .map(str::to_owned)
                    .to_vec()
            ),
            "rustc_lexer::strip_shebang decides which `#!` is a shebang"
        );
    }

    #[test]
    fn tokens_carry_the_line_they_start_on() {
        let source = Source::new("x\r\na\r\n\"x\r\ny\"\nb");
        let lexed = source.tokens().expect("terminated");

        let lines: Vec<u64> = lexed.iter().map(|lexed| lexed.line.get()).collect();
        assert_eq!(
            lines,
            [1, 2, 3, 5],
            "a multi-line literal starts on 3, and the token after it is on 5"
        );
    }

    #[test]
    fn a_byte_order_mark_goes_and_crlf_becomes_lf() {
        let source = Source::new("\u{FEFF}a\r\n\"b\r\nc\"\rd");

        assert_eq!(
            (source.text(), texts("\u{FEFF}a\r\n\"b\r\nc\"\rd")),
            (
                "a\n\"b\nc\"\rd",
                ["a", "\"b\nc\"", "d"].map(str::to_owned).to_vec()
            ),
            "rustc's normalisation; a lone CR stays, as whitespace"
        );
    }

    #[test]
    fn a_shebang_is_told_from_an_inner_attribute_as_rustc_tells_them() {
        let attribute = ["#", "!", "[", "x", "]"].map(str::to_owned).to_vec();
        let shebang = Err(Unlexable(NonZeroU64::MIN));

        assert_eq!(
            (
                texts("#! // c\n[x]"),
                texts("#!\u{0B}\u{0C}[x]"),
                Source::new("#! /// c\n[x]").tokens(),
                Source::new("#! /** c */ [x]").tokens(),
                Source::new("#! /* open\n[x]").tokens(),
            ),
            (
                attribute.clone(),
                attribute,
                shebang.clone(),
                shebang.clone(),
                shebang,
            ),
            "plain comments and whitespace are skipped; a doc comment or an open one ends it"
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
                Source::new(text).tokens(),
                Err(Unlexable(NonZeroU64::MIN.saturating_add(1))),
                "{text}"
            );
        }
    }

    #[test]
    fn code_outside_ascii_and_frontmatter_are_errors_at_their_line() {
        for text in [
            "a\nb\u{200E}",
            "a\n\u{2028}",
            "a\nx\u{B7}y",
            "a\n'\u{2118} ",
            "a\n'a\u{E9}",
            "a\n1\u{E9}",
            "a\n\"s\"\u{E9}",
            "a\nr#\u{E9}",
            "\n---\n",
            " \n---\n",
        ] {
            assert_eq!(
                Source::new(text).tokens(),
                Err(Unlexable(NonZeroU64::MIN.saturating_add(1))),
                "{text}"
            );
        }
    }

    #[test]
    fn literals_and_comments_may_hold_any_text() {
        assert_eq!(
            texts("'\u{2118}' \"\u{200E}\" /* \u{85} */ // \u{E9}\nb'\u{E9}' x"),
            ["'\u{2118}'", "\"\u{200E}\"", "b'\u{E9}'", "x"].map(str::to_owned),
            "the boundaries are ASCII"
        );
    }
}
