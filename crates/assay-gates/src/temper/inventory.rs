//! Step 4a, the test inventory of one `.rs` file (`docs/architecture/03-contracts.md` §2.1):
//! what its tokens declare about tests, read from #63's lexer rather than a syntax tree, so a
//! test inside a macro call (`proptest! { #[test] .. }`) counts too.
//!
//! Nothing is expanded (Weigh-In default wi-b5affa). A test a `macro_rules!` transcriber writes
//! counts once however many times the macro is called; the per-file comparison then sees the
//! calls only as they change the file.

use std::collections::BTreeMap;

use super::attributes::group;
use super::reach;
use super::references::{self, Reference};
use super::tokens::{Lexed, Token};

mod runs;

use runs::{Attribute, gates_module, run};

/// What one file declares about its tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Inventory {
    /// Test-family attributes under no condition: none in their own run of attributes, none
    /// on an item around them.
    pub(super) tests: u64,
    /// How many test-family attributes sit under each set of conditions, keyed by those
    /// conditions as one normalised text, outermost first: a multiset, so a test that goes, or
    /// whose condition changes, is a loss.
    pub(super) conditional: BTreeMap<String, u64>,
    /// Attributes that can ignore a test: `ignore` itself, and any attribute naming `ignore`
    /// in its arguments (`cfg_attr(miri, ignore)`, `test_case(.. => ignore)`).
    pub(super) ignores: u64,
    /// The text of each condition (`cfg` or `cfg_attr`, except exactly `cfg(test)`) on a
    /// module or written as an inner attribute, which can switch off every test below it.
    pub(super) gates: Vec<String>,
    /// The name of each top-level `mod name;` without `#[path]`, owned since the file's text is
    /// read only to count it.
    pub(super) modules: Vec<String>,
    /// Whether the reach scan could not place every module of the file (a `#[path]` past its
    /// window, a `cfg_attr(.., path = ..)`, a form it does not read): any name in `modules` may
    /// then name another file than its own, so dropping one is refused.
    pub(super) modules_uncertain: bool,
    /// Each module reference this check cannot follow by name: a `#[path]` module and each
    /// reference `references` cannot resolve. Dropping one is refused.
    pub(super) unplaced_modules: Vec<Unplaced>,
}

/// A module reference this check cannot follow by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Unplaced {
    /// `mod name;` under `#[path = "path"]`.
    Path { name: String, path: String },
    /// `mod name;` in a file whose modules the reach scan could not all place. Only a loss
    /// names one; the inventory keeps such a name in `modules` and sets `modules_uncertain`.
    Uncertain(String),
    /// A reference `references` does not resolve, by kind.
    Reference(ReferenceKind),
}

/// The kinds of reference `references` does not resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ReferenceKind {
    /// An include macro call, or a `mod` inside brackets or a macro (`Reference::Unresolvable`).
    Unresolvable,
    /// A `macro_rules` inside brackets or another macro body.
    NestedMacro,
}

/// Most conditioned items a run of attributes can sit inside before the file is refused (H-13).
const MAX_DEPTH: usize = 16;

/// Most bytes of condition text one file's inventory builds before it is refused (H-13).
const MAX_CONDITION_BYTES: usize = 1 << 20;

/// The file asks for more work than the inventory's fixed budget, so it is not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Refused;

/// The work one file's inventory may still do.
struct Budget {
    /// Tokens the item scans may still walk.
    scan: usize,
    /// Bytes of condition text that may still be built.
    text: usize,
}

impl Budget {
    fn spend(left: &mut usize, cost: usize) -> Result<(), Refused> {
        *left = left.checked_sub(cost).ok_or(Refused)?;
        Ok(())
    }
}

/// The inventory of a file whose tokens are `tokens`, or [`Refused`] past the work budget:
/// more than [`MAX_DEPTH`] nested conditioned items, more than [`MAX_CONDITION_BYTES`] of
/// condition text, or item scans walking more than `MAX_DEPTH + 2` times the file's tokens.
pub(super) fn inventory(tokens: &[Lexed<'_>]) -> Result<Inventory, Refused> {
    let (modules, modules_uncertain, unplaced_modules) = declared_modules(tokens);
    let mut found = Inventory {
        modules,
        modules_uncertain,
        unplaced_modules,
        ..Inventory::default()
    };
    let mut budget = Budget {
        scan: tokens.len().saturating_mul(MAX_DEPTH.saturating_add(2)),
        text: MAX_CONDITION_BYTES,
    };
    // Each conditioned item's conditions, its enclosing items' first, built once; and how many
    // tests sit under each.
    let mut chains: Vec<(String, u64)> = Vec::new();
    // The conditioned items being read: where each ends, and its index in `chains`.
    let mut scopes: Vec<(usize, usize)> = Vec::new();
    let mut index = 0_usize;
    while let Some((first, tail)) = tokens.get(index..).and_then(<[_]>::split_first) {
        scopes.retain(|(end, _)| *end > index);
        index = index.saturating_add(1);
        if first.token != Token::Punct('#') {
            continue;
        }
        let (run, after) = run(tail);
        if run.is_empty() {
            continue;
        }
        index = tokens.len().saturating_sub(after.len());
        let outer = scopes.last().map(|(_, chain)| *chain);
        let chain = match condition_text(&run) {
            None => outer,
            Some(own) => {
                if scopes.len() >= MAX_DEPTH {
                    return Err(Refused);
                }
                let outer_text = outer
                    .and_then(|chain| chains.get(chain))
                    .map(|(text, _)| text);
                let text = match outer_text {
                    Some(outer_text) => format!("{outer_text} & {own}"),
                    None => own,
                };
                Budget::spend(&mut budget.text, text.len())?;
                let end = index.saturating_add(item_length(after, &mut budget.scan)?);
                chains.push((text, 0));
                let chain = chains.len().saturating_sub(1);
                scopes.push((end, chain));
                Some(chain)
            }
        };
        tally(
            &mut found,
            &run,
            chain.and_then(|chain| chains.get_mut(chain)),
            gates_module(after),
        );
    }
    for (text, tests) in chains.into_iter().filter(|(_, tests)| *tests > 0) {
        let count = found.conditional.entry(text).or_default();
        *count = count.saturating_add(tests);
    }
    Ok(found)
}

/// The conditions in `run`'s own outer attributes, as one text; `None` when it holds none.
fn condition_text(run: &[Attribute<'_, '_>]) -> Option<String> {
    let own: Vec<String> = run
        .iter()
        .filter(|attribute| !attribute.inner && attribute.is_condition())
        .map(Attribute::text)
        .collect();
    (!own.is_empty()).then(|| own.join(" & "))
}

/// The top-level `mod name;` declarations of a file, by name; whether the reach scan was
/// uncertain of the file's modules; and every module reference this check cannot place by
/// name: a `#[path]` module (its name and path), and each reference `references` cannot
/// resolve (a `mod` in brackets or a macro, an include, a nested macro), by kind. A reference's
/// line is left out: lines move between base and head, and the comparison is by value.
fn declared_modules(tokens: &[Lexed<'_>]) -> (Vec<String>, bool, Vec<Unplaced>) {
    let reach = reach::scan(tokens);
    let mut placed = Vec::new();
    let mut unplaced = Vec::new();
    for (name, path) in reach.modules {
        match path {
            None => placed.push(name),
            Some(path) => unplaced.push(Unplaced::Path { name, path }),
        }
    }
    unplaced.extend(
        references::references(tokens)
            .into_iter()
            .filter_map(|(_, reference)| match reference {
                Reference::Module(_) => None,
                Reference::Unresolvable => Some(Unplaced::Reference(ReferenceKind::Unresolvable)),
                Reference::NestedMacro => Some(Unplaced::Reference(ReferenceKind::NestedMacro)),
            }),
    );
    (placed, reach.uncertain, unplaced)
}

/// The number of tokens of the item `after` starts: up to its first `;` or through its first
/// `{ .. }` outside other brackets, or to the end; each token walked is spent from `left`.
///
/// A `{` inside generics (`S<{ N }>`) ends it early, so the tests after it read as under no
/// condition and a condition later added on that item is not seen (a known gap).
fn item_length(after: &[Lexed<'_>], left: &mut usize) -> Result<usize, Refused> {
    let mut index = 0_usize;
    while let Some(lexed) = after.get(index) {
        let next = match lexed.token {
            Token::Punct(';') => return Ok(index.saturating_add(1)),
            Token::Punct('(' | '[' | '{') => {
                let inner = after.get(index.saturating_add(1)..).unwrap_or(&[]);
                index
                    .saturating_add(group(lexed, inner).len())
                    .saturating_add(2)
            }
            _ => index.saturating_add(1),
        };
        Budget::spend(left, next.saturating_sub(index))?;
        if lexed.token == Token::Punct('{') {
            return Ok(next);
        }
        index = next;
    }
    Ok(after.len())
}

/// Counts one run of attributes, under the conditions of `chain` when it has any (the items'
/// around it, then its own); its item is a module when `module` holds.
fn tally(
    found: &mut Inventory,
    run: &[Attribute<'_, '_>],
    mut chain: Option<&mut (String, u64)>,
    module: bool,
) {
    for attribute in run {
        if attribute.is_test() && !attribute.inner {
            match chain.as_deref_mut() {
                None => found.tests = found.tests.saturating_add(1),
                Some((_, tests)) => *tests = tests.saturating_add(1),
            }
        }
        if attribute.is_ignore() {
            found.ignores = found.ignores.saturating_add(1);
        }
        if attribute.is_condition() && (attribute.inner || module) {
            found.gates.push(attribute.text());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tokens::Source;
    use super::*;

    fn of(text: &str) -> (u64, u64, Vec<String>, Vec<String>) {
        let source = Source::new(text);
        let lexed = source.tokens().expect("terminated");
        let found = inventory(&lexed).expect("within the budget");
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
    fn a_test_under_a_conditioned_item_is_keyed_by_every_condition() {
        let source = Source::new(
            "#[cfg(a)]\nmod m {\n    #[cfg(b)]\n    #[test]\n    fn t() {}\n}\n#[test]\nfn u() {}\n",
        );
        let lexed = source.tokens().expect("terminated");

        let found = inventory(&lexed).expect("within the budget");

        assert_eq!(
            (found.tests, found.conditional),
            (1, BTreeMap::from([("cfg ( a ) & cfg ( b )".to_owned(), 1)])),
            "`t` is under both, outermost first; `u` is after `m` ends"
        );
    }

    #[test]
    fn an_item_ends_at_its_semicolon_or_its_body() {
        let source = Source::new("f!(x, [y]); fn g() -> [u8; 2] { [0; 2] } z");
        let lexed = source.tokens().expect("terminated");
        let rest = lexed.get(10..).expect("the call is ten tokens");
        let mut left = usize::MAX;

        assert_eq!(
            (item_length(&lexed, &mut left), item_length(rest, &mut left)),
            (Ok(10), Ok(18)),
            "a `;` in brackets does not end an item; the body does"
        );
    }

    #[test]
    fn an_item_scan_past_its_budget_is_refused() {
        let source = Source::new("a b c d e f g;");
        let lexed = source.tokens().expect("terminated");
        let mut left = 3;

        assert_eq!(
            item_length(&lexed, &mut left),
            Err(Refused),
            "the `;` is past three tokens"
        );
    }

    #[test]
    fn conditions_nested_past_the_depth_are_refused() {
        let nested = "#[cfg(a)] x\n".repeat(MAX_DEPTH.saturating_add(1));
        let flat = "#[cfg(a)] fn x() {}\n".repeat(MAX_DEPTH.saturating_add(1));
        let refused = |text: &str| {
            let source = Source::new(text);
            inventory(&source.tokens().expect("terminated")).err()
        };

        assert_eq!(
            (refused(&nested), refused(&flat)),
            (Some(Refused), None),
            "an item without `;` or `{{` runs to the end, so the next nests in it"
        );
    }

    #[test]
    fn condition_text_past_its_budget_is_refused() {
        let long = format!("#[cfg({})] fn x() {{}}\n", "a".repeat(MAX_CONDITION_BYTES));
        let source = Source::new(&long);

        assert_eq!(
            inventory(&source.tokens().expect("terminated")),
            Err(Refused),
            "one condition longer than the budget"
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
