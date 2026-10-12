//! Attributes read from the head's Rust tokens: the ones the pinned `temper scan` does not read
//! (03 §2.1 step 2), and the lints each `#[expect(..)]` names, since temper reports only the
//! first.

use std::num::NonZeroU64;

use super::tokens::{Lexed, Token};

/// Identifiers that make a `cfg_attr` hide a lint suppression or a module path from the scan.
const HIDDEN: [&str; 3] = ["allow", "expect", "path"];

/// Attributes the scan reads only with their argument list: written `name $x`, a macro call
/// can make them anything.
const LISTED: [&str; 6] = ["allow", "expect", "warn", "deny", "forbid", "cfg_attr"];

/// The macros that read another file into this one.
const INCLUDES: [&str; 3] = ["include", "include_str", "include_bytes"];

/// The lines of attributes the scan does not read, once each:
/// - a `cfg_attr(..)` whose arguments name `allow`, `expect` or `path` anywhere (raw or not),
///   or a macro fragment `$x`, which could be any of them;
/// - a `#[path ..]` or `#![path ..]`, which can compile a file the scan never walks;
/// - an `allow`, `expect`, `warn`, `deny`, `forbid` or `cfg_attr` attribute whose name is not
///   followed by `(`;
/// - an attribute named by a raw identifier (`#[r#allow(..)]`), which temper's patterns do not
///   match;
/// - an attribute built from a macro fragment: `#[$m]`, `# $m`, or a fragment or repetition
///   followed by `[` or `! [` (`$hash [allow(x)]`), which a macro call can fill with any of
///   these;
/// - an `include!` of anything but one literal naming a `.rs` file, and an `include_str!` or
///   `include_bytes!` of anything but one literal.
pub(super) fn unread(tokens: &[Lexed<'_>]) -> Vec<NonZeroU64> {
    let mut lines = Vec::new();
    let mut rest = tokens;
    while let [first, tail @ ..] = rest {
        let hidden = match (first.token, tail) {
            (
                Token::Ident {
                    name: "cfg_attr", ..
                },
                [open, inner @ ..],
            ) if open.token == Token::Punct('(') => {
                group(inner).iter().any(|lexed| hides(lexed.token))
            }
            (Token::Ident { name, .. }, [bang, after @ ..])
                if INCLUDES.contains(&name) && bang.token == Token::Punct('!') =>
            {
                !included(name, after)
            }
            (Token::Punct('#'), _) => match attribute(tail) {
                Some(Attribute::Fragment) => true,
                Some(Attribute::Named { name, raw, rest }) => {
                    raw || name == "path" || (LISTED.contains(&name) && !opens(rest, '('))
                }
                None => false,
            },
            (Token::Punct('$'), _) => fragment_before_bracket(tail),
            _ => false,
        };
        if hidden && lines.last() != Some(&first.line) {
            lines.push(first.line);
        }
        rest = tail;
    }
    lines
}

/// Whether `tokens` starts with the punctuation `punct`.
fn opens(tokens: &[Lexed<'_>], punct: char) -> bool {
    tokens
        .first()
        .is_some_and(|lexed| lexed.token == Token::Punct(punct))
}

/// Whether the call of `name` (one of [`INCLUDES`]), from the tokens after its `!`, reads a file
/// the scan accounts for: one literal, which for `include!` names a `.rs` file.
fn included(name: &str, after_bang: &[Lexed<'_>]) -> bool {
    let [open, inner @ ..] = after_bang else {
        return false;
    };
    if !matches!(open.token, Token::Punct('(' | '[' | '{')) {
        return false;
    }
    let target = match group(inner) {
        [only]
        | [
            only,
            Lexed {
                token: Token::Punct(','),
                ..
            },
        ] => only.token,
        _ => return false,
    };
    match target {
        Token::Literal(literal) => name != "include" || names_rust_source(literal),
        Token::Ident { .. } | Token::Lifetime(_) | Token::Punct(_) => false,
    }
}

/// Whether `literal` is a string literal, plain without escapes or raw, whose text ends `.rs`.
fn names_rust_source(literal: &str) -> bool {
    let quoted = match literal.strip_prefix('r') {
        Some(raw) => raw.trim_matches('#'),
        None if literal.contains('\\') => return false,
        None => literal,
    };
    quoted.len() >= 2 && quoted.starts_with('"') && quoted.ends_with(".rs\"")
}

/// Whether the tokens after a `$` are a fragment (`$x`) or a repetition (`$(..)*`, `$(..),+`)
/// followed by `[` or `! [`: an attribute whose `#` the macro call supplies.
fn fragment_before_bracket(after_dollar: &[Lexed<'_>]) -> bool {
    let after = match after_dollar {
        [name, rest @ ..] if matches!(name.token, Token::Ident { .. }) => rest,
        [open, inner @ ..] if open.token == Token::Punct('(') => {
            let Some(after_group) = inner.get(group(inner).len().saturating_add(1)..) else {
                return false;
            };
            past_operator(after_group)
        }
        _ => return false,
    };
    match after {
        [bang, rest @ ..] if bang.token == Token::Punct('!') => opens(rest, '['),
        rest => opens(rest, '['),
    }
}

/// The tokens after a repetition's optional separator and its operator (`*`, `+` or `?`).
fn past_operator<'t, 'a>(after_group: &'t [Lexed<'a>]) -> &'t [Lexed<'a>] {
    let operator = |lexed: &Lexed<'_>| matches!(lexed.token, Token::Punct('*' | '+' | '?'));
    match after_group {
        [op, rest @ ..] if operator(op) => rest,
        [_, op, rest @ ..] if operator(op) => rest,
        rest => rest,
    }
}

/// Whether a token inside a `cfg_attr` can make it hide something from the scan.
fn hides(token: Token<'_>) -> bool {
    match token {
        Token::Ident { name, .. } => HIDDEN.contains(&name),
        Token::Punct(punct) => punct == '$',
        Token::Lifetime(_) | Token::Literal(_) => false,
    }
}

/// One `#[expect(..)]` or `#![expect(..)]`: each argument but `reason = ..`, as the lint path
/// it spells (`clippy::too_many_lines`), or `None` for an argument that is not a path.
pub(super) type Expect = Vec<Option<String>>;

/// Every `expect` attribute named plainly and followed by `(`, and the line of its `#`. Any
/// other `expect` (`#[r#expect(..)]`, `#[expect $x]`) is one [`unread`] reports instead.
pub(super) fn expects(tokens: &[Lexed<'_>]) -> Vec<(NonZeroU64, Expect)> {
    let mut found = Vec::new();
    let mut rest = tokens;
    while let [first, tail @ ..] = rest {
        if first.token == Token::Punct('#')
            && let Some(Attribute::Named {
                name: "expect",
                raw: false,
                rest: [open, inner @ ..],
            }) = attribute(tail)
            && open.token == Token::Punct('(')
        {
            found.push((first.line, lints(group(inner))));
        }
        rest = tail;
    }
    found
}

/// What a `#` starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attribute<'t, 'a> {
    /// `#[name ..]` or `#![name ..]`, and the tokens after the name.
    Named {
        name: &'a str,
        raw: bool,
        rest: &'t [Lexed<'a>],
    },
    /// `#[$x ..]`, `#![$x ..]`, `# $x` or `#! $x`: an attribute a macro fragment supplies.
    Fragment,
}

/// The attribute the tokens after a `#` start, if any.
fn attribute<'t, 'a>(after_hash: &'t [Lexed<'a>]) -> Option<Attribute<'t, 'a>> {
    let after_bang = match after_hash {
        [bang, rest @ ..] if bang.token == Token::Punct('!') => rest,
        rest => rest,
    };
    let after_open = match after_bang {
        [open, rest @ ..] if open.token == Token::Punct('[') => rest,
        [dollar, ..] if dollar.token == Token::Punct('$') => return Some(Attribute::Fragment),
        _ => return None,
    };
    match after_open {
        [name, rest @ ..] => match name.token {
            Token::Ident { name, raw } => Some(Attribute::Named { name, raw, rest }),
            Token::Punct('$') => Some(Attribute::Fragment),
            Token::Punct(_) | Token::Lifetime(_) | Token::Literal(_) => None,
        },
        [] => None,
    }
}

/// The tokens inside a group whose opening bracket has been consumed, up to its matching
/// close; all of `inner` when it never closes.
fn group<'t, 'a>(inner: &'t [Lexed<'a>]) -> &'t [Lexed<'a>] {
    let mut depth = 0_usize;
    for (index, lexed) in inner.iter().enumerate() {
        match lexed.token {
            Token::Punct('(' | '[' | '{') => depth = depth.saturating_add(1),
            Token::Punct(')' | ']' | '}') => {
                if depth == 0 {
                    return inner.get(..index).unwrap_or(inner);
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    inner
}

/// The lint arguments of an `expect`'s argument list.
fn lints(arguments: &[Lexed<'_>]) -> Expect {
    let mut lints = Vec::new();
    let mut depth = 0_usize;
    let mut current: Vec<Token<'_>> = Vec::new();
    for lexed in arguments {
        match lexed.token {
            Token::Punct(',') if depth == 0 => {
                push_lint(&mut lints, &current);
                current.clear();
                continue;
            }
            Token::Punct('(' | '[' | '{') => depth = depth.saturating_add(1),
            Token::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            _ => {}
        }
        current.push(lexed.token);
    }
    push_lint(&mut lints, &current);
    lints
}

/// Adds one argument to `lints`, unless it is empty (a trailing comma) or `reason = ..`: the
/// path it spells, or `None` when it is something else. A raw segment keeps its `r#`, so it
/// never matches the lint temper names.
fn push_lint(lints: &mut Expect, argument: &[Token<'_>]) {
    match argument {
        []
        | [
            Token::Ident {
                name: "reason",
                raw: false,
            },
            Token::Punct('='),
            ..,
        ] => {}
        tokens => lints.push(
            tokens
                .iter()
                .map(|token| match *token {
                    Token::Ident { name, raw: false } => Some(name.to_owned()),
                    Token::Ident { name, raw: true } => Some(format!("r#{name}")),
                    Token::Punct(':') => Some(":".to_owned()),
                    Token::Punct(_) | Token::Lifetime(_) | Token::Literal(_) => None,
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tokens::Source;
    use super::*;

    fn line(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("lines start at 1")
    }

    /// The lines [`unread`] reports in `text`.
    fn unread_in(text: &str) -> Vec<NonZeroU64> {
        unread(&Source::new(text).tokens().expect("terminated"))
    }

    #[test]
    fn reads_every_lint_of_an_expect_but_its_reason() {
        let source = Source::new(
            "#[expect(clippy :: a, b, reason = \"x, y\")]\n#![expect(c, d(e, f), 3)] #[expect(g,)]",
        );
        let lexed = source.tokens().expect("terminated");

        assert_eq!(
            expects(&lexed),
            [
                (
                    line(1),
                    vec![Some("clippy::a".to_owned()), Some("b".to_owned())]
                ),
                (line(2), vec![Some("c".to_owned()), None, None]),
                (line(2), vec![Some("g".to_owned())])
            ],
            "commas inside the reason or a group do not split; a non-path is None"
        );
    }

    #[test]
    fn a_hidden_suppression_in_a_macro_fragment_is_unread() {
        assert_eq!(
            unread_in("#[cfg_attr(all(), $m)]\nfn f() {}"),
            [line(1)],
            "`$m` could expand to `allow(..)`"
        );
    }

    #[test]
    fn a_string_cannot_close_the_cfg_attr_early() {
        assert_eq!(
            unread_in("#[cfg_attr(all(), doc = \")\", allow(x))]"),
            [line(1)],
            "the `)` is inside a literal"
        );
    }

    #[test]
    fn a_line_is_reported_once_however_many_attributes_hide_on_it() {
        assert_eq!(
            unread_in("#[r#cfg_attr(all(), allow(x))] #[path = \"a.rs\"]\n#[path = \"b.rs\"]"),
            [line(1), line(2)],
            "one finding per line"
        );
    }

    #[test]
    fn an_include_reads_one_literal_naming_rust_source() {
        assert_eq!(
            [
                "include!(\"a.rs\");",
                "include!(r##\"a.rs\"##,);",
                "include_str!(\"a.json\");",
                "include_bytes!(b\"a\");",
                "fn include() {} include();",
                "include!(\"a.rs\", \"b.rs\");",
                "include!(\"a.rs\".x);",
                "include!(r\"a.rs\"x);",
                "include!(b\"a.rs\");",
                "include!;",
            ]
            .map(|text| unread_in(text).len()),
            [0, 0, 0, 0, 0, 1, 1, 1, 1, 1],
            "a literal for the data macros; a `.rs` literal for `include!`"
        );
    }

    #[test]
    fn a_fragment_is_an_attribute_only_before_a_bracket() {
        assert_eq!(
            [
                "$h [x]",
                "$h![x]",
                "$($h)?[x]",
                "$($h);*[x]",
                "$h:tt [x]",
                "$h (x)",
                "$($h)* (x)",
                "$crate::x",
            ]
            .map(|text| unread_in(text).len()),
            [1, 1, 1, 1, 0, 0, 0, 0],
            "`$h [` can be `# [`"
        );
    }
}
