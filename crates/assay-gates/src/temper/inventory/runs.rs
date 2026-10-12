//! Runs of attributes as the inventory reads them: each attribute's path and arguments, and
//! whether the item after a run declares a module.

use crate::temper::attributes::group;
use crate::temper::tokens::{Lexed, Token};

/// Attribute names, by their last path segment, that mark a test besides any name holding
/// `test` (`test`, `tokio::test`, `rstest`, `test_case`, `wasm_bindgen_test`): `bench`, and
/// `case`, each of which is one more `rstest` case.
const TEST_NAMES: [&str; 2] = ["bench", "case"];

/// One attribute: its path's segments and its arguments.
pub(super) struct Attribute<'t, 'a> {
    pub(super) inner: bool,
    path: Vec<&'a str>,
    arguments: &'t [Lexed<'a>],
    tokens: &'t [Lexed<'a>],
}

impl Attribute<'_, '_> {
    fn last(&self) -> &str {
        self.path.last().copied().unwrap_or("")
    }

    pub(super) fn is_test(&self) -> bool {
        let last = self.last();
        last.contains("test") || TEST_NAMES.contains(&last)
    }

    fn names(&self, ident: &str) -> bool {
        self.arguments
            .iter()
            .any(|lexed| matches!(lexed.token, Token::Ident { name, .. } if name == ident))
    }

    pub(super) fn is_ignore(&self) -> bool {
        self.last() == "ignore" || self.names("ignore")
    }

    /// A `cfg` but exactly `cfg(test)`, which holds in every test build, or a `cfg_attr` that
    /// names `cfg` or a macro fragment anywhere in its arguments, either of which can make it a
    /// `cfg`. A `cfg_attr` naming `path` swaps a module's file instead; step 2 reports it as a
    /// `blind-spot`, which no waiver clears.
    pub(super) fn is_condition(&self) -> bool {
        match self.path.as_slice() {
            ["cfg"] => !matches!(
                self.arguments,
                [only] if matches!(only.token, Token::Ident { name: "test", .. })
            ),
            ["cfg_attr"] => {
                self.names("cfg")
                    || self
                        .arguments
                        .iter()
                        .any(|lexed| lexed.token == Token::Punct('$'))
            }
            _ => false,
        }
    }

    /// The attribute as text, its tokens separated by spaces.
    pub(super) fn text(&self) -> String {
        self.tokens
            .iter()
            .map(|lexed| match lexed.token {
                Token::Ident { name, raw: false } => name.to_owned(),
                Token::Ident { name, raw: true } => format!("r#{name}"),
                Token::Lifetime(name) => format!("'{name}"),
                Token::Punct(punct) => punct.to_string(),
                Token::Literal(text) => text.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The attributes that start right after a `#` (whose own tokens are `after_hash`), each
/// following one `#` after the first, and the tokens after the last. An inner attribute is a
/// run of its own.
pub(super) fn run<'t, 'a>(
    after_hash: &'t [Lexed<'a>],
) -> (Vec<Attribute<'t, 'a>>, &'t [Lexed<'a>]) {
    let mut found = Vec::new();
    let mut rest = after_hash;
    while let Some((attribute, after)) = attribute(rest) {
        let inner = attribute.inner;
        found.push(attribute);
        rest = after;
        match rest {
            [hash, tail @ ..] if !inner && hash.token == Token::Punct('#') => {
                if matches!(tail.first(), Some(bang) if bang.token == Token::Punct('!')) {
                    break;
                }
                rest = tail;
            }
            _ => break,
        }
    }
    if found.is_empty() {
        return (found, after_hash);
    }
    (found, rest)
}

/// The attribute the tokens after a `#` start, and the tokens after its `]`.
fn attribute<'t, 'a>(after_hash: &'t [Lexed<'a>]) -> Option<(Attribute<'t, 'a>, &'t [Lexed<'a>])> {
    let (inner, after_bang) = match after_hash {
        [bang, rest @ ..] if bang.token == Token::Punct('!') => (true, rest),
        rest => (false, rest),
    };
    let [open, inside @ ..] = after_bang else {
        return None;
    };
    if open.token != Token::Punct('[') {
        return None;
    }
    let tokens = group(inside);
    let after = inside.get(tokens.len().saturating_add(1)..).unwrap_or(&[]);
    let mut path = Vec::new();
    let mut rest = tokens;
    loop {
        rest = match rest {
            [colon, second, tail @ ..]
                if colon.token == Token::Punct(':') && second.token == Token::Punct(':') =>
            {
                tail
            }
            _ => rest,
        };
        match rest {
            [segment, tail @ ..] => match segment.token {
                Token::Ident { name, .. } => {
                    path.push(name);
                    rest = tail;
                }
                Token::Punct(_) | Token::Lifetime(_) | Token::Literal(_) => break,
            },
            [] => break,
        }
        if !matches!(rest, [colon, ..] if colon.token == Token::Punct(':')) {
            break;
        }
    }
    let arguments = match rest {
        [open, inner @ ..] if matches!(open.token, Token::Punct('(' | '[' | '{')) => group(inner),
        rest => rest,
    };
    let attribute = Attribute {
        inner,
        path,
        arguments,
        tokens,
    };
    Some((attribute, after))
}

/// Whether the tokens after a run of outer attributes declare a module: `mod`, after an
/// optional visibility (`pub`, `pub(crate)`, `pub(in path)`).
pub(super) fn gates_module(after: &[Lexed<'_>]) -> bool {
    let after_visibility = match after {
        [visibility, open, rest @ ..]
            if matches!(visibility.token, Token::Ident { name: "pub", .. })
                && open.token == Token::Punct('(') =>
        {
            rest.get(group(rest).len().saturating_add(1)..)
                .unwrap_or(&[])
        }
        [visibility, rest @ ..] if matches!(visibility.token, Token::Ident { name: "pub", .. }) => {
            rest
        }
        rest => rest,
    };
    matches!(
        after_visibility.first(),
        Some(Lexed {
            token: Token::Ident { name: "mod", .. },
            ..
        })
    )
}
