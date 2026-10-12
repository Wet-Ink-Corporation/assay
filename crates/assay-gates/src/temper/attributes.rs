//! Attributes read from the head's Rust tokens: the ones the pinned `temper scan` does not read
//! (03 §2.1 step 2), and the lints each `#[expect(..)]` names, since temper reports only the
//! first.

use std::num::NonZeroU64;

use super::tokens::{Lexed, Token};

/// Identifiers that make a `cfg_attr` hide a lint suppression or a module path from the scan.
const HIDDEN: [&str; 3] = ["allow", "expect", "path"];

/// The lines of attributes the scan does not read: a `cfg_attr(..)` whose arguments name
/// `allow`, `expect` or `path` anywhere (or a macro fragment `$x`, which could be any of them),
/// and a `#[path ..]` or `#![path ..]`, which can compile a file the scan never walks.
pub(super) fn unread(tokens: &[Lexed<'_>]) -> Vec<NonZeroU64> {
    let mut lines = Vec::new();
    let mut rest = tokens;
    while let [first, tail @ ..] = rest {
        let hidden = match (first.token, tail) {
            (Token::Ident("cfg_attr"), [open, inner @ ..]) => {
                open.token == Token::Punct('(')
                    && group(inner).iter().any(|lexed| hides(lexed.token))
            }
            (Token::Punct('#'), _) => attribute(tail).is_some_and(|(name, _)| name == "path"),
            _ => false,
        };
        if hidden {
            lines.push(first.line);
        }
        rest = tail;
    }
    lines
}

/// Whether a token inside a `cfg_attr` can make it hide something from the scan.
fn hides(token: Token<'_>) -> bool {
    match token {
        Token::Ident(word) => HIDDEN.contains(&word),
        Token::Punct(punct) => punct == '$',
        Token::Literal => false,
    }
}

/// One `#[expect(..)]` or `#![expect(..)]`: each argument but `reason = ..`, as the lint path
/// it spells (`clippy::too_many_lines`), or `None` for an argument that is not a path.
pub(super) type Expect = Vec<Option<String>>;

/// Every `expect` attribute with an argument list, and the line of its `#`.
pub(super) fn expects(tokens: &[Lexed<'_>]) -> Vec<(NonZeroU64, Expect)> {
    let mut found = Vec::new();
    let mut rest = tokens;
    while let [first, tail @ ..] = rest {
        if first.token == Token::Punct('#')
            && let Some(("expect", [open, inner @ ..])) = attribute(tail)
            && open.token == Token::Punct('(')
        {
            found.push((first.line, lints(group(inner))));
        }
        rest = tail;
    }
    found
}

/// The name of the attribute a `#` starts, `#[name ..]` or `#![name ..]`, and the tokens after
/// the name.
fn attribute<'t, 'a>(after_hash: &'t [Lexed<'a>]) -> Option<(&'a str, &'t [Lexed<'a>])> {
    let after_bang = match after_hash {
        [bang, rest @ ..] if bang.token == Token::Punct('!') => rest,
        rest => rest,
    };
    match after_bang {
        [open, name, rest @ ..] if open.token == Token::Punct('[') => match name.token {
            Token::Ident(name) => Some((name, rest)),
            Token::Punct(_) | Token::Literal => None,
        },
        _ => None,
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
/// path it spells, or `None` when it is something else.
fn push_lint(lints: &mut Expect, argument: &[Token<'_>]) {
    match argument {
        [] | [Token::Ident("reason"), Token::Punct('='), ..] => {}
        tokens => lints.push(
            tokens
                .iter()
                .map(|token| match token {
                    Token::Ident(word) => Some(*word),
                    Token::Punct(':') => Some(":"),
                    Token::Punct(_) | Token::Literal => None,
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tokens::tokens;
    use super::*;

    fn line(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("lines start at 1")
    }

    #[test]
    fn reads_every_lint_of_an_expect_but_its_reason() {
        let lexed = tokens(
            "#[expect(clippy :: a, b, reason = \"x, y\")]\n#![expect(c, d(e, f), 3)] #[expect(g,)]",
        )
        .expect("terminated");

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
        let lexed = tokens("#[cfg_attr(all(), $m)]\nfn f() {}").expect("terminated");

        assert_eq!(
            unread(&lexed),
            [line(1)],
            "`$m` could expand to `allow(..)`"
        );
    }

    #[test]
    fn a_string_cannot_close_the_cfg_attr_early() {
        let lexed = tokens("#[cfg_attr(all(), doc = \")\", allow(x))]").expect("terminated");

        assert_eq!(unread(&lexed), [line(1)], "the `)` is inside a literal");
    }
}
