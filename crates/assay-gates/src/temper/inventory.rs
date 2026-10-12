//! Step 4a, the test inventory of one `.rs` file (`docs/architecture/03-contracts.md` §2.1):
//! what its tokens declare about tests, read from #63's lexer rather than a syntax tree, so a
//! test inside a macro call (`proptest! { #[test] .. }`) counts too.
//!
//! Nothing is expanded (Weigh-In default wi-b5affa). A test a `macro_rules!` transcriber writes
//! counts once however many times the macro is called; the per-file comparison then sees the
//! calls only as they change the file.

use super::attributes::group;
use super::references::{self, Reference};
use super::tokens::{Lexed, Token};

/// Attribute names, by their last path segment, that mark a test besides any name holding
/// `test` (`test`, `tokio::test`, `rstest`, `test_case`, `wasm_bindgen_test`): `bench`, and
/// `case`, each of which is one more `rstest` case.
const TEST_NAMES: [&str; 2] = ["bench", "case"];

/// What one file declares about its tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Inventory {
    /// Test-family attributes outside any run of attributes holding a condition.
    pub(super) tests: u64,
    /// Attributes that can ignore a test: `ignore` itself, and any attribute naming `ignore`
    /// in its arguments (`cfg_attr(miri, ignore)`, `test_case(.. => ignore)`).
    pub(super) ignores: u64,
    /// The text of each condition (`cfg` or `cfg_attr`, except exactly `cfg(test)`) on a
    /// module or written as an inner attribute, which can switch off every test below it.
    pub(super) gates: Vec<String>,
    /// The name of each top-level `mod name;`, owned since the file's text is read only to
    /// count it.
    pub(super) modules: Vec<String>,
}

/// One attribute: its path's segments and its arguments.
struct Attribute<'t, 'a> {
    inner: bool,
    path: Vec<&'a str>,
    arguments: &'t [Lexed<'a>],
    tokens: &'t [Lexed<'a>],
}

impl Attribute<'_, '_> {
    fn last(&self) -> &str {
        self.path.last().copied().unwrap_or("")
    }

    fn is_test(&self) -> bool {
        let last = self.last();
        last.contains("test") || TEST_NAMES.contains(&last)
    }

    fn names(&self, ident: &str) -> bool {
        self.arguments
            .iter()
            .any(|lexed| matches!(lexed.token, Token::Ident { name, .. } if name == ident))
    }

    fn is_ignore(&self) -> bool {
        self.last() == "ignore" || self.names("ignore")
    }

    /// A `cfg` but exactly `cfg(test)`, which holds in every test build, or a `cfg_attr` that
    /// names `cfg` or a macro fragment anywhere in its arguments, either of which can make it a
    /// `cfg`. A `cfg_attr` naming `path` swaps a module's file instead; step 2 reports it as a
    /// `blind-spot`, which no waiver clears.
    fn is_condition(&self) -> bool {
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
    fn text(&self) -> String {
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

/// The inventory of a file whose tokens are `tokens`.
pub(super) fn inventory(tokens: &[Lexed<'_>]) -> Inventory {
    let mut found = Inventory {
        modules: references::references(tokens)
            .into_iter()
            .filter_map(|(_, reference)| match reference {
                Reference::Module(name) => Some(name.to_owned()),
                Reference::Unresolvable | Reference::NestedMacro => None,
            })
            .collect(),
        ..Inventory::default()
    };
    let mut rest = tokens;
    while let [first, tail @ ..] = rest {
        rest = tail;
        if first.token != Token::Punct('#') {
            continue;
        }
        let (run, after) = run(tail);
        if run.is_empty() {
            continue;
        }
        rest = after;
        tally(&mut found, &run, gates_module(after));
    }
    found
}

/// Counts one run of attributes, whose item is a module when `module` holds.
fn tally(found: &mut Inventory, run: &[Attribute<'_, '_>], module: bool) {
    let conditional = run.iter().any(Attribute::is_condition);
    for attribute in run {
        if attribute.is_test() && !conditional && !attribute.inner {
            found.tests = found.tests.saturating_add(1);
        }
        if attribute.is_ignore() {
            found.ignores = found.ignores.saturating_add(1);
        }
        if attribute.is_condition() && (attribute.inner || module) {
            found.gates.push(attribute.text());
        }
    }
}

/// The attributes that start right after a `#` (whose own tokens are `after_hash`), each
/// following one `#` after the first, and the tokens after the last. An inner attribute is a
/// run of its own.
fn run<'t, 'a>(after_hash: &'t [Lexed<'a>]) -> (Vec<Attribute<'t, 'a>>, &'t [Lexed<'a>]) {
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
fn gates_module(after: &[Lexed<'_>]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::super::tokens::Source;
    use super::*;

    fn of(text: &str) -> (u64, u64, Vec<String>, Vec<String>) {
        let source = Source::new(text);
        let lexed = source.tokens().expect("terminated");
        let found = inventory(&lexed);
        (found.tests, found.ignores, found.gates, found.modules)
    }

    #[test]
    fn counts_tests_ignores_and_modules() {
        assert_eq!(
            of(
                "mod a;\n#[test] #[ignore] fn f() {}\n#[tokio::test(flavor = \"x\")] async fn g() {}"
            ),
            (2, 1, Vec::new(), vec!["a".to_owned()]),
            "two tests, one ignored, one module"
        );
    }

    #[test]
    fn a_condition_in_the_run_makes_its_tests_conditional() {
        assert_eq!(
            of("#[test]\n#[cfg(unix)]\nfn f() {}\n#[cfg(test)]\n#[test]\nfn g() {}"),
            (1, 0, Vec::new(), Vec::new()),
            "`cfg(unix)` can switch the first off; `cfg(test)` cannot"
        );
    }

    #[test]
    fn records_conditions_on_modules_and_inner_conditions() {
        assert_eq!(
            of(
                "#![cfg(unix)]\n#[cfg(test)] mod a {}\n#[cfg(x)] pub(crate) mod b;\n#[cfg(y)] fn f() {}"
            ),
            (
                0,
                0,
                vec!["cfg ( unix )".to_owned(), "cfg ( x )".to_owned()],
                vec!["b".to_owned()]
            ),
            "a condition on a function is not a module's"
        );
    }

    #[test]
    fn an_inner_attribute_ends_a_run() {
        assert_eq!(
            of("#![cfg(x)]\n#[test]\nfn f() {}"),
            (1, 0, vec!["cfg ( x )".to_owned()], Vec::new()),
            "the inner condition gates the file, the test is still counted"
        );
    }
}
