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

/// The macros that read another file into this one; `references.rs` judges their calls.
pub(super) const INCLUDES: [&str; 3] = ["include", "include_str", "include_bytes"];

/// The lines of attributes the scan does not read, once each:
/// - a `cfg_attr(..)` whose arguments name `allow`, `expect` or `path` anywhere (raw or not),
///   or a macro fragment `$x`, which could be any of them;
/// - a `#[path ..]` or `#![path ..]`, which can compile a file the scan never walks;
/// - an `allow`, `expect`, `warn`, `deny`, `forbid` or `cfg_attr` attribute whose name is not
///   followed by `(`;
/// - an attribute named by a raw identifier (`#[r#allow(..)]`), which temper's patterns do not
///   match;
/// - a `#` not followed by `[`, `! [` or `$`, and `#[$m]` or `# $m`: an attribute whose
///   brackets or path a macro call supplies;
/// - a fragment `$x`, or a repetition `$(..)op` that can end with one, followed by `[`, `$`,
///   `#` or `!`: an attribute a macro call can assemble with no `#` beside its brackets
///   (`$hash $list`), or a call of a macro a fragment names (`$m!`);
/// - `include`, `include_str` or `include_bytes` not called with `!` (`use std::include as
///   inc;`), which renames a macro `references.rs` judges.
pub(super) fn unread(tokens: &[Lexed<'_>]) -> Vec<NonZeroU64> {
    let mut lines: Vec<NonZeroU64> = unread_why(tokens)
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    lines.dedup();
    lines
}

/// Why [`unread`] reports a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unread {
    /// A `cfg_attr` naming `path` or a macro fragment, which can make it a `#[path]`.
    CfgAttrPath,
    /// A `cfg_attr` naming `allow` or `expect` only.
    CfgAttrLint,
    /// An include macro not called with `!`.
    RenamedInclude,
    /// `#[path ..]` or `#![path ..]`.
    Path,
    /// A lint or `cfg_attr` attribute whose name is not followed by `(`.
    Listless,
    /// An attribute named by a raw identifier.
    Raw,
    /// A `#` that does not start an attribute, or an attribute a fragment names.
    Fragment,
    /// A fragment or repetition that can assemble an attribute or name a macro.
    Assembled,
}

/// Every token [`unread`] reports, at its line, with why, in order.
pub(super) fn unread_why(tokens: &[Lexed<'_>]) -> Vec<(NonZeroU64, Unread)> {
    let mut found = Vec::new();
    let mut rest = tokens;
    while let [first, tail @ ..] = rest {
        let why = match (first.token, tail) {
            (
                Token::Ident {
                    name: "cfg_attr", ..
                },
                [open, inner @ ..],
            ) if open.token == Token::Punct('(') => cfg_attr(group(inner)),
            (Token::Ident { name, .. }, _) if INCLUDES.contains(&name) => {
                (!opens(tail, '!')).then_some(Unread::RenamedInclude)
            }
            (Token::Punct('#'), _) => match attribute(tail) {
                Some(Attribute::Fragment) | None => Some(Unread::Fragment),
                Some(Attribute::Named { raw: true, .. }) => Some(Unread::Raw),
                Some(Attribute::Named { name: "path", .. }) => Some(Unread::Path),
                Some(Attribute::Named { name, rest, .. }) => {
                    (LISTED.contains(&name) && !opens(rest, '(')).then_some(Unread::Listless)
                }
            },
            (Token::Punct('$'), _) => assembles(tail).then_some(Unread::Assembled),
            _ => None,
        };
        if let Some(why) = why {
            found.push((first.line, why));
        }
        rest = tail;
    }
    found
}

/// Why a `cfg_attr` with these arguments is unread, if it is.
fn cfg_attr(arguments: &[Lexed<'_>]) -> Option<Unread> {
    let names_path = arguments.iter().any(|lexed| match lexed.token {
        Token::Ident { name, .. } => name == "path",
        Token::Punct(punct) => punct == '$',
        Token::Lifetime(_) | Token::Literal(_) => false,
    });
    if names_path {
        Some(Unread::CfgAttrPath)
    } else {
        arguments
            .iter()
            .any(|lexed| hides(lexed.token))
            .then_some(Unread::CfgAttrLint)
    }
}

/// Whether `tokens` starts with the punctuation `punct`.
fn opens(tokens: &[Lexed<'_>], punct: char) -> bool {
    tokens
        .first()
        .is_some_and(|lexed| lexed.token == Token::Punct(punct))
}

/// Whether the tokens after a `$` are a fragment (`$x`, not `$crate`), or a repetition
/// (`$(..)*`, `$(..),+`) that can end with one, followed by `[`, `$`, `#` or `!`. A macro call
/// can bind the fragment to `#` and the next to `[allow(..)]`, or name a macro with it.
fn assembles(after_dollar: &[Lexed<'_>]) -> bool {
    let after = match after_dollar {
        [fragment, rest @ ..] => match fragment.token {
            Token::Ident { name: "crate", .. } => return false,
            Token::Ident { .. } => rest,
            Token::Punct('(') => {
                let body = group(rest);
                if !open_ended(body) {
                    return false;
                }
                let Some(after_group) = rest.get(body.len().saturating_add(1)..) else {
                    return false;
                };
                past_operator(after_group)
            }
            Token::Punct(_) | Token::Lifetime(_) | Token::Literal(_) => return false,
        },
        [] => return false,
    };
    after
        .first()
        .is_some_and(|next| matches!(next.token, Token::Punct('[' | '$' | '#' | '!')))
}

/// Whether a repetition's expansion can end with a token a macro call chose or with a lone
/// punctuation mark: unless its body ends with a closing bracket, a plain identifier or a
/// literal.
fn open_ended(body: &[Lexed<'_>]) -> bool {
    match body {
        [] => false,
        [.., before, last] if matches!(last.token, Token::Ident { .. }) => {
            before.token == Token::Punct('$')
        }
        [.., last] => match last.token {
            Token::Punct(')' | ']' | '}') | Token::Ident { .. } | Token::Literal(_) => false,
            Token::Punct(_) | Token::Lifetime(_) => true,
        },
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
    listed(tokens, "expect")
        .map(|(line, arguments)| (line, lints(arguments)))
        .collect()
}

/// The line of the `#` of every `allow` attribute named plainly and followed by `(`, which the
/// scan's `allow-attr` pattern is meant to report there. Any other `allow` is one [`unread`]
/// reports instead.
pub(super) fn allows(tokens: &[Lexed<'_>]) -> Vec<NonZeroU64> {
    listed(tokens, "allow").map(|(line, _)| line).collect()
}

/// Every attribute named plainly `name` and followed by `(`: the line of its `#` and its
/// arguments.
fn listed<'t, 'a>(
    tokens: &'t [Lexed<'a>],
    name: &'static str,
) -> impl Iterator<Item = (NonZeroU64, &'t [Lexed<'a>])> {
    tokens.iter().enumerate().filter_map(move |(index, first)| {
        if first.token != Token::Punct('#') {
            return None;
        }
        let tail = tokens.get(index.checked_add(1)?..)?;
        match attribute(tail) {
            Some(Attribute::Named {
                name: named,
                raw: false,
                rest: [open, inner @ ..],
            }) if named == name && open.token == Token::Punct('(') => {
                Some((first.line, group(inner)))
            }
            _ => None,
        }
    })
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
pub(super) fn group<'t, 'a>(inner: &'t [Lexed<'a>]) -> &'t [Lexed<'a>] {
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
    fn finds_every_plain_allow_with_its_list_at_the_line_of_its_hash() {
        // Assembled, so this file's own scan does not report the text as an attribute.
        let text = concat!(
            "#[al",
            "low(a)] #![al",
            "low(b)]\n#/* c */\n[al",
            "low(d)]\n",
            "#[r#al",
            "low(e)] #[al",
            "low = \"f\"] #[warn(g)] #[cfg_attr(x, al",
            "low(h))]"
        );
        let source = Source::new(text);
        let lexed = source.tokens().expect("terminated");

        assert_eq!(
            allows(&lexed),
            [line(1), line(1), line(2)],
            "a raw or list-less one is unread's; one inside a cfg_attr is too"
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
    fn an_include_macro_is_read_only_where_it_is_called() {
        assert_eq!(
            [
                "include!(\"a.rs\");",
                "include_str!(\"a.json\");",
                "use std::include;",
                "use std::include_bytes as bytes;",
                "fn include() {}",
            ]
            .map(|text| unread_in(text).len()),
            [0, 0, 1, 1, 1],
            "any other use can rename it"
        );
    }

    #[test]
    fn a_fragment_that_can_end_in_a_hash_cannot_precede_a_list_a_hash_or_a_bang() {
        assert_eq!(
            [
                "$h [x]",
                "$h![x]",
                "$h $a",
                "$h #",
                "$m!(x)",
                "$($h)?[x]",
                "$($h);*[x]",
                "$($h)* $a",
                "$(x,)* $a",
                "$h:tt [x]",
                "$h (x)",
                "$($h)* (x)",
                "$crate::x",
                "$(#[doc = $d])* $v",
                "$(x)* $a",
                "$(1)* #[x]",
            ]
            .map(|text| unread_in(text).len()),
            [1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
            "`$h` can be `#` and `$a` can be `[allow(..)]`"
        );
    }

    #[test]
    fn a_hash_starts_an_attribute_or_a_fragment() {
        assert_eq!(
            [
                "#[x]", "#![x]", "# ! [x]", "#$a", "#", "#, [x]", "# ! x", "#[]", "#[::x]"
            ]
            .map(|text| unread_in(text).len()),
            [0, 0, 0, 1, 1, 1, 1, 1, 1],
            "a `#` a macro passes on can head an attribute assembled elsewhere"
        );
    }
}
