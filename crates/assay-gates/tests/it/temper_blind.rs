//! What the pinned `temper scan --all` cannot see fails the gate instead (03 §2.1 step 2):
//! `.rs` files under the directories its walk skips or behind a symlinked directory, and
//! attributes it does not read (`cfg_attr` carrying `allow`, `expect` or `path`, and `#[path]`).

use std::error::Error;

use assay_app::{GateJobOutcome, Termination};
use assay_domain::{GateConclusion, Severity};
use assay_gates::Revision;

use crate::temper_support::{Pr, Seen, hit};

const FAILED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Fail);
const PASSED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Pass);

/// A package manifest, inheriting the workspace lints as step 4d requires of every package.
const PACKAGE: &str = "[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n";

fn blind(file: &str, line: u64) -> Seen {
    (
        "blind-spot".to_owned(),
        file.to_owned(),
        line,
        Severity::Blocker,
    )
}

#[test]
fn rust_under_a_directory_the_scan_skips_fails() -> Result<(), Box<dyn Error>> {
    for file in [
        "crates/x/target/gen.rs",
        "target/debug/build.rs",
        "web/node_modules/pkg/lib.rs",
        "crates/x/.temper/hidden.rs",
        "a/.git/b.rs",
    ] {
        let mut pr = Pr::new()?;
        pr.set(Revision::Head, file, "pub fn f() {}\n");

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![blind(file, 1)]), "{file}");
    }
    Ok(())
}

#[test]
fn a_non_rust_file_under_a_skipped_directory_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "web/node_modules/pkg/index.js", "x\n");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "only Rust can be compiled in"
    );
    Ok(())
}

#[test]
fn a_submodule_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.submodule(Revision::Head, "vendor/dep");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("vendor/dep", 1)]),
        "a recursive checkout compiles Rust the scan never saw"
    );
    Ok(())
}

#[test]
fn a_dangling_symlink_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.link(Revision::Head, "src/gone.rs", "missing.rs");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/gone.rs", 1)]),
        "nothing to read"
    );
    Ok(())
}

#[test]
fn a_rust_symlink_to_a_file_that_is_not_rust_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "notes.txt",
        "#[cfg_attr(all(), allow(unused))]\n",
    );
    pr.link(Revision::Head, "src/notes.rs", "../notes.txt");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/notes.rs", 1)]),
        "rustc compiles the text file, which no `.rs` check reads"
    );
    Ok(())
}

#[test]
fn a_symlink_through_a_parent_of_a_symlink_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.rs", "pub fn f() {}\n");
    pr.link(Revision::Head, "src/up", "lib.rs");
    pr.link(Revision::Head, "src/via.rs", "up/../lib.rs");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/via.rs", 1)]),
        "`up/..` is not `src` once `up` is followed"
    );
    Ok(())
}

#[test]
fn rust_that_does_not_lex_fails_at_the_open_literal() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "fn f() {}\nconst S: &str = \"open;\n",
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/lib.rs", 2)]),
        "its attributes cannot be read"
    );
    Ok(())
}

#[test]
fn brackets_nested_past_the_cap_fail_quickly() -> Result<(), Box<dyn Error>> {
    let depth = 30_000;
    let nested = format!(
        "fn f() {{}}\nx!({}{});\n",
        "cfg_attr(".repeat(depth),
        ")".repeat(depth)
    );
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.rs", &nested);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/lib.rs", 2)]),
        "past 256 open brackets the file is not read, so no group is walked again and again"
    );
    Ok(())
}

#[test]
fn a_cfg_attr_nested_in_another_fails_quickly() -> Result<(), Box<dyn Error>> {
    // `#[` and 255 `cfg_attr(`: 256 open brackets, the deepest the lexer reads.
    let levels = 255;
    let nested = format!(
        "fn f() {{}}\n#[{}{}{}]\nfn g() {{}}\n",
        "cfg_attr(unix, ".repeat(levels),
        "x, ".repeat(500_000),
        ")".repeat(levels)
    );
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.rs", &nested);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/lib.rs", 2)]),
        "a nested `cfg_attr` is refused, not read again at every level"
    );
    Ok(())
}

#[test]
fn a_symlink_to_a_directory_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "elsewhere/hidden.rs", "pub fn f() {}\n");
    pr.link(Revision::Head, "src/inner", "../elsewhere");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/inner", 1)]),
        "the scan's walk does not follow it, but rustc does"
    );
    Ok(())
}

#[test]
fn a_symlink_that_leaves_the_tree_fails() -> Result<(), Box<dyn Error>> {
    for target in ["/etc", "../../outside", "a/../../.."] {
        let mut pr = Pr::new()?;
        pr.link(Revision::Head, "src/link", target);

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![blind("src/link", 1)]), "{target}");
    }
    Ok(())
}

#[test]
fn a_symlink_to_a_file_in_the_tree_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "docs/guide.md", "# guide\n");
    pr.link(Revision::Head, "README.md", "docs/guide.md");

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "the scan reads through it");
    Ok(())
}

#[test]
fn a_symlinked_rust_file_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.rs", "pub fn f() {}\n");
    pr.link(Revision::Head, "src/alias.rs", "lib.rs");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/alias.rs", 1)]),
        "rustc and the scan need not agree on which file a module is (wi-b5affa)"
    );
    Ok(())
}

#[test]
fn attributes_the_scan_does_not_read_fail_at_their_line() -> Result<(), Box<dyn Error>> {
    for attribute in [
        "#[cfg_attr(test, allow(clippy::unwrap_used))]",
        "#![cfg_attr(not(test), expect(dead_code, reason = \"r\"))]",
        "#[cfg_attr(\n    feature = \"x\",\n    allow(unused)\n)]",
        "#[cfg_attr(unix, path = \"unix.rs\")]",
        "#[path = \"evil.txt\"]",
        "# ! [ path = \"evil.txt\" ]",
    ] {
        let mut pr = Pr::new()?;
        let source = format!("// one\n{attribute}\nmod m;\n");
        pr.set(Revision::Head, "src/lib.rs", &source);
        pr.set(Revision::Head, "src/m.rs", "fn f() {}\n");
        pr.set(Revision::Head, "Cargo.toml", PACKAGE);

        let outcome = pr.outcome()?;

        assert_eq!(
            outcome,
            (FAILED, vec![blind("src/lib.rs", 2)]),
            "{attribute}"
        );
    }
    Ok(())
}

#[test]
fn a_cfg_attr_without_lints_or_paths_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "#[cfg_attr(test, derive(Debug))]\n#[cfg_attr(doc, doc = \"allowed paths\")]\n\
         pub struct S;\nfn path() {}\n",
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "`allowed`, `paths` and a function named `path` are not the attributes"
    );
    Ok(())
}

/// A gate outcome and its findings, as `Pr::outcome` returns them.
type Outcome = (GateJobOutcome, Vec<Seen>);

/// The outcome of a head whose `src/lib.rs` is `source`, by `source`, for every source.
fn outcomes<'s>(sources: &[&'s str]) -> Result<Vec<(&'s str, Outcome)>, Box<dyn Error>> {
    sources
        .iter()
        .map(|source| {
            let mut pr = Pr::new()?;
            pr.set(Revision::Head, "src/lib.rs", source);
            Ok((*source, pr.outcome()?))
        })
        .collect()
}

#[test]
fn no_literal_lifetime_or_shebang_hides_an_attribute_from_the_lexer() -> Result<(), Box<dyn Error>>
{
    // A lexer that reads `r"` on line 1 as a raw string (no escapes) stays inside a string
    // over line 2 and resynchronises on line 3, so the hidden `cfg_attr` would pass unseen.
    // temper's own masker is such a lexer, so each line 1 is refused where it starts: a name
    // ending in `r` against a quote, or a shebang (wi-b5affa).
    let hidden = "\n#[cfg_attr(all(), allow(unused))]\nconst S: &str = \"\\\"\";\n";
    let sources: Vec<String> = [
        "m!('r\"\\\"\");",
        "m!(\"x\"r\"\\\"\");",
        "m!('a'r\"\\\"\");",
        "m!(b'a'r\"\\\"\");",
        "m!(r#r\"\\\"\");",
        "m!('br\"\\\"\");",
        "#!/usr/bin/env run \"",
    ]
    .iter()
    .map(|first| format!("{first}{hidden}"))
    .collect();
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();

    let seen = outcomes(&sources)?;

    let expected: Vec<_> = sources
        .iter()
        .map(|source| (*source, (FAILED, vec![blind("src/lib.rs", 1)])))
        .collect();
    assert_eq!(seen, expected, "line 2's cfg_attr is code to rustc");
    Ok(())
}

/// Line 2 of every source below: an attribute the checks must see, then a line closing any
/// string a desynchronised lexer opened on line 1.
const HIDDEN: &str = "\n#[cfg_attr(all(), allow(unused))]\nconst S: &str = \"\\\"\";\n";

/// `sources`, each expected to fail with one `blind-spot` at `line`.
fn each_blind_at(sources: &[(&str, u64)]) -> Vec<(String, Outcome)> {
    sources
        .iter()
        .map(|(source, line)| {
            (
                (*source).to_owned(),
                (FAILED, vec![blind("src/lib.rs", *line)]),
            )
        })
        .collect()
}

/// The outcome of each of `sources`, keyed by its text.
fn owned_outcomes(sources: &[(&str, u64)]) -> Result<Vec<(String, Outcome)>, Box<dyn Error>> {
    let texts: Vec<&str> = sources.iter().map(|(source, _)| *source).collect();
    Ok(outcomes(&texts)?
        .into_iter()
        .map(|(source, outcome)| (source.to_owned(), outcome))
        .collect())
}

#[test]
fn code_outside_ascii_fails_where_it_starts() -> Result<(), Box<dyn Error>> {
    // rustc reads LRM as whitespace and `x·r`, `'℘` as an identifier and a lifetime, by
    // Unicode tables this lexer does not carry: any non-ASCII character outside a literal or
    // comment is read as a blind spot instead (wi-6c128e).
    let skip = format!("skip!(x\u{B7}r\"\\\"\");{HIDDEN}");
    let lifetime = format!("m!('\u{2118} '\\\"');{HIDDEN}");
    let sources = [
        (
            "// one\n#[cfg_attr\u{200E}(all(), allow(x))]\nfn f() {}\n",
            2,
        ),
        ("// one\n#\u{200E}[allow(x)]\nfn f() {}\n", 2),
        ("#!\u{200E}[allow(x)]\nfn f() {}\n", 1),
        ("// one\nfn f() {}\u{2028}\n", 2),
        ("// one\nfn f() {}\u{85}\n", 2),
        ("// one\nfn f\u{200F}() {}\n", 2),
        ("// one\nfn caf\u{E9}() {}\n", 2),
        (skip.as_str(), 1),
        (lifetime.as_str(), 1),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "rustc lexes these by Unicode rules"
    );
    Ok(())
}

#[test]
fn text_in_comments_and_literals_may_be_any_unicode() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "/// caf\u{E9} \u{200E}\nconst C: char = '\u{2118}';\nconst S: &str = \"\u{2028}\";\n\
         /* \u{85} */ fn f() {}\n",
    );

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "only code must be ASCII");
    Ok(())
}

#[test]
fn a_byte_order_mark_or_frontmatter_hides_nothing() -> Result<(), Box<dyn Error>> {
    let bom = format!("\u{FEFF}#!/bin/x r\"{HIDDEN}");
    let crlf = "m!(r\"x\"\r\n);\r\n#[cfg_attr(all(), allow(unused))]\r\nfn f() {}\r\n";
    let frontmatter = format!("---\nx = \"\n---{HIDDEN}");
    let indented = format!("\n  ---\nx = \"\n---{HIDDEN}");
    let sources = [
        (bom.as_str(), 1),
        (crlf, 3),
        (frontmatter.as_str(), 1),
        (indented.as_str(), 2),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "rustc drops the mark and the shebang, and reads `---` first as frontmatter"
    );
    Ok(())
}

#[test]
fn a_lint_attribute_without_its_argument_list_fails() -> Result<(), Box<dyn Error>> {
    let sources = [
        (
            "macro_rules! m {\n    ($a:tt) => { #[allow $a] fn f() {} };\n}\n",
            2,
        ),
        (
            "macro_rules! m {\n    ($a:tt) => { #[expect $a] fn f() {} };\n}\n",
            2,
        ),
        (
            "macro_rules! m {\n    ($a:tt) => { #[cfg_attr $a] fn f() {} };\n}\n",
            2,
        ),
        ("macro_rules! m {\n    ($a:tt) => { #![warn $a] };\n}\n", 2),
        (
            "macro_rules! m {\n    ($a:tt) => { #[deny $a] fn f() {} };\n}\n",
            2,
        ),
        (
            "macro_rules! m {\n    ($a:tt) => { #[forbid $a] fn f() {} };\n}\n",
            2,
        ),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "`$a` can be `(unused)`, which no scan reads"
    );
    Ok(())
}

#[test]
fn a_fragment_before_a_bracket_fails() -> Result<(), Box<dyn Error>> {
    let sources = [
        (
            "macro_rules! m {\n    ($h:tt) => { $h [allow(x)] fn f() {} };\n}\n",
            2,
        ),
        (
            "macro_rules! m {\n    ($h:tt) => { $h ! [allow(x)] };\n}\n",
            2,
        ),
        (
            "macro_rules! m {\n    ($($h:tt)*) => { $($h)* [allow(x)] fn f() {} };\n}\n",
            2,
        ),
        (
            "macro_rules! m {\n    ($($h:tt),*) => { $($h),* [allow(x)] fn f() {} };\n}\n",
            2,
        ),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "`m!(#)` makes each an attribute with no `#` in the source"
    );
    Ok(())
}

#[test]
fn an_include_of_anything_but_named_rust_source_fails() -> Result<(), Box<dyn Error>> {
    let sources = [
        ("// one\ninclude!(\"gen.txt\");\n", 2),
        ("// one\ninclude!(\"gen.r\\x73\");\n", 2),
        ("// one\ninclude!(\"gen.rs\"x);\n", 2),
        (
            "// one\ninclude!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n",
            2,
        ),
        (
            "// one\nconst S: &str = include_str!(concat!(\"a\", \".txt\"));\n",
            2,
        ),
        ("// one\nconst B: &[u8] = include_bytes!(env!(\"X\"));\n", 2),
        ("// one\nconst B: &[u8] = include_bytes![];\n", 2),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "rustc compiles or embeds a file the scan never reads"
    );
    Ok(())
}

#[test]
fn an_include_fails_whatever_it_names() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "include!(\"gen.rs\");\n// two\ninclude! { r#\"gen.rs\"#, }\n",
    );
    pr.set(Revision::Head, "src/gen.rs", "fn g() {}\n");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/lib.rs", 1), blind("src/lib.rs", 3)]),
        "the checks refuse rather than model rustc's path resolution (wi-b5affa)"
    );
    Ok(())
}

#[test]
fn a_data_include_of_a_literal_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "const S: &str = include_str!(\"schema.json\");\n\
         const B: &[u8] = include_bytes!(\"logo.png\");\n",
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "data, never code (wi-855abd)"
    );
    Ok(())
}

/// A head holding `files`, each `(path, text)`.
fn outcome_of(files: &[(&str, &str)]) -> Result<Outcome, Box<dyn Error>> {
    let mut pr = Pr::new()?;
    for (path, text) in files {
        pr.set(Revision::Head, path, text);
    }
    pr.outcome()
}

#[test]
fn an_include_that_does_not_resolve_to_a_scanned_rust_file_fails() -> Result<(), Box<dyn Error>> {
    let cases: [&[(&str, &str)]; 6] = [
        &[("src/lib.rs", "// one\ninclude!(\"/etc/x.rs\");\n")],
        &[("src/lib.rs", "// one\ninclude!(\"../../x.rs\");\n")],
        &[("src/lib.rs", "// one\ninclude!(\"gen.rs\");\n")],
        &[
            ("src/lib.rs", "// one\ninclude!(\"c.rs\");\n"),
            ("c.rs", "fn c() {}\n"),
        ],
        &[
            ("src/a/b.rs", "// one\ninclude!(\"c.rs\");\n"),
            ("src/c.rs", "fn c() {}\n"),
        ],
        &[
            ("src/lib.rs", "// one\ninclude!(\"gen/../out.rs\");\n"),
            ("src/out.rs", "fn c() {}\n"),
        ],
    ];

    let seen: Vec<Outcome> = cases
        .iter()
        .map(|files| outcome_of(files))
        .collect::<Result<_, _>>()?;

    let expected: Vec<Outcome> = cases
        .iter()
        .map(|files| (FAILED, vec![blind(files[0].0, 2)]))
        .collect();
    assert_eq!(
        seen, expected,
        "absolute, escaping, missing (build output), resolved from the wrong directory, or \
         through a directory the tree does not have"
    );
    Ok(())
}

#[test]
fn an_include_of_a_skipped_file_fails_at_both_ends() -> Result<(), Box<dyn Error>> {
    let outcome = outcome_of(&[
        ("src/lib.rs", "// one\ninclude!(\"target/gen.rs\");\n"),
        ("src/target/gen.rs", "fn g() {}\n"),
    ])?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![blind("src/lib.rs", 2), blind("src/target/gen.rs", 1)]
        ),
        "the scan does not read the included file"
    );
    Ok(())
}

#[test]
fn an_include_the_scan_cannot_pin_to_a_file_fails() -> Result<(), Box<dyn Error>> {
    let sources = [
        ("// one\nuse std::include as inc;\n", 2),
        ("// one\nuse std::{include_str, include_bytes};\n", 2),
        ("// one\nuse std::fs as include;\n", 2),
        (
            "macro_rules! m {\n    ($m:ident) => { $m!(\"x.txt\") };\n}\n",
            2,
        ),
        (
            "macro_rules! m {\n    () => { include!(\"gen.rs\"); };\n}\n",
            2,
        ),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "a renamed include, a macro named by a fragment, or an include resolved from a caller"
    );
    Ok(())
}

#[test]
fn a_module_without_its_file_in_the_tree_fails() -> Result<(), Box<dyn Error>> {
    const MANIFEST: (&str, &str) = ("Cargo.toml", PACKAGE);
    let cases: [&[(&str, &str)]; 8] = [
        &[("src/lib.rs", "// one\nmod generated;\n"), MANIFEST],
        &[
            ("src/a.rs", "// one\nmod b;\n"),
            ("src/b.rs", "fn b() {}\n"),
            ("src/a/c.rs", "fn c() {}\n"),
        ],
        &[
            ("src/lib.rs", "// one\nmod inner {\n    mod deep;\n}\n"),
            MANIFEST,
        ],
        &[
            (
                "src/lib.rs",
                "macro_rules! m {\n    ($n:ident) => { mod $n; };\n}\n",
            ),
            MANIFEST,
        ],
        &[
            ("src/lib.rs", "// one\nm!(\n    mod hidden;\n);\n"),
            MANIFEST,
        ],
        &[
            (
                "src/lib.rs",
                "// one\nconst X: [u8; 0] = [\n    mod hidden;\n];\n",
            ),
            MANIFEST,
        ],
        // No manifest beside `src`, so `lib.rs` is not a crate root: `mod a;` is `src/lib/a.rs`.
        &[
            ("src/lib.rs", "// one\nmod a;\n"),
            ("src/a.rs", "fn a() {}\n"),
        ],
        // `main.rs` declares `lib.rs` a module too, so `mod x;` there can be either file.
        &[
            ("src/lib.rs", "// one\nmod x;\n"),
            ("src/main.rs", "mod lib;\n"),
            ("src/x.rs", "fn x() {}\n"),
            MANIFEST,
        ],
    ];

    let seen: Vec<Outcome> = cases
        .iter()
        .map(|files| outcome_of(files))
        .collect::<Result<_, _>>()?;

    let expected: Vec<Outcome> = [2, 2, 3, 2, 3, 3, 2, 2]
        .into_iter()
        .zip(&cases)
        .map(|(line, files)| (FAILED, vec![blind(files[0].0, line)]))
        .collect();
    assert_eq!(
        seen, expected,
        "a build script can write the file rustc compiles after the scan ran"
    );
    Ok(())
}

#[test]
fn a_module_with_its_file_in_the_tree_passes() -> Result<(), Box<dyn Error>> {
    let outcome = outcome_of(&[
        (
            "src/lib.rs",
            "pub mod a;\nmod m;\n#[cfg(test)]\nmod tests {}\n",
        ),
        ("src/a.rs", "mod b;\n"),
        ("src/a/b.rs", "mod c;\n"),
        ("src/a/b/c/mod.rs", "fn c() {}\n"),
        ("src/m/mod.rs", "fn m() {}\n"),
        ("src/bin/tool.rs", "mod helper;\n"),
        ("src/bin/helper.rs", "fn h() {}\n"),
        ("tests/it/main.rs", "mod support;\n"),
        ("tests/it/support.rs", "fn s() {}\n"),
        ("tests/smoke.rs", "mod common;\n"),
        ("tests/common/mod.rs", "fn c() {}\n"),
        ("Cargo.toml", PACKAGE),
    ])?;

    assert_eq!(outcome, (PASSED, Vec::new()), "rustc's module paths");
    Ok(())
}

#[test]
fn a_root_also_declared_a_module_passes_with_both_files() -> Result<(), Box<dyn Error>> {
    let outcome = outcome_of(&[
        ("src/lib.rs", "mod x;\n"),
        ("src/main.rs", "mod lib;\n"),
        ("src/x.rs", "fn x() {}\n"),
        ("src/lib/x.rs", "fn x() {}\n"),
        ("Cargo.toml", PACKAGE),
    ])?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "either reading of `lib.rs` finds a scanned file"
    );
    Ok(())
}

#[test]
fn a_plain_allow_the_scan_did_not_report_fails() -> Result<(), Box<dyn Error>> {
    let sources = [
        ("// one\n#[allow(dead_code)]\nfn f() {}\n", 2),
        ("// one\n#![allow(clippy::unwrap_used)]\n", 2),
        ("// one\n# [\n    allow(x)\n]\nfn f() {}\n", 2),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "temper's masker missed it, so its baseline does not count it"
    );
    Ok(())
}

#[test]
fn a_plain_allow_the_scan_reported_is_left_to_the_baseline() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "// one\n#[allow(dead_code)]\nfn f() {}\n",
    );
    pr.scan.termination = Termination::Exited(1);
    pr.scan_prints(&[hit("allow-attr", ("src/lib.rs", 2), "hard", None)]);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "a hard hit is gates-lint's `baseline --check`"
    );
    Ok(())
}

#[test]
fn a_plain_allow_after_a_shebang_or_a_name_ending_in_r_fails_at_line_1()
-> Result<(), Box<dyn Error>> {
    let shebang = "#!/usr/bin/env run \"\n#![allow(clippy::unwrap_used, reason = \"x\")]\n";
    let allow = "\n#[allow(clippy::unwrap_used)]\nfn f() {}\n";
    let lifetime = format!("m!('r\"\\\"\");{allow}");
    let suffix = format!("m!(\"x\"r\"\\\"\");{allow}");
    let raw = format!("m!(r#r\"\\\"\");{allow}");
    let sources = [
        (shebang, 1),
        (lifetime.as_str(), 1),
        (suffix.as_str(), 1),
        (raw.as_str(), 1),
        ("m!('br#x);\n", 1),
        ("m!(1r\"x\");\n", 1),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "temper's masker reads `r\"` there as a raw string"
    );
    Ok(())
}

#[test]
fn an_attribute_assembled_by_a_macro_fails() -> Result<(), Box<dyn Error>> {
    let cases = [
        (
            "macro_rules! m {\n    ($h:tt, $a:tt) => { $h $a fn f() {} };\n}\n\
             m!(#, [allow(dead_code)]);\n",
            vec![2, 4],
        ),
        (
            "macro_rules! m {\n    ($h:tt $d:tt $a:tt) => { $h $a fn f() {} };\n}\n\
             m! {\n    /// x\n    [allow(dead_code)]\n}\n",
            vec![2],
        ),
        (
            "macro_rules! m {\n    ($h:tt $a:tt) => { $h ! $a };\n}\n",
            vec![2],
        ),
        (
            "macro_rules! m {\n    ($h:tt) => { $h #[doc = \"\"] fn f() {} };\n}\n",
            vec![2],
        ),
        (
            "macro_rules! m {\n    ($($h:tt)*) => { $($h)* $($h)* };\n}\n",
            vec![2],
        ),
        ("// one\nm!(# ! x);\n", vec![2]),
        ("// one\nm!(#);\n", vec![2]),
    ];

    let sources: Vec<&str> = cases.iter().map(|(source, _)| *source).collect();
    let seen = outcomes(&sources)?;

    let expected: Vec<_> = cases
        .iter()
        .map(|(source, lines)| {
            (
                *source,
                (
                    FAILED,
                    lines
                        .iter()
                        .map(|line| blind("src/lib.rs", *line))
                        .collect(),
                ),
            )
        })
        .collect();
    assert_eq!(
        seen, expected,
        "a `#` from the call site and a list from a fragment make an attribute"
    );
    Ok(())
}

#[test]
fn a_macro_definition_below_the_top_level_fails() -> Result<(), Box<dyn Error>> {
    // Both compile under -D warnings: the inner transcriber's `$d h [allow(..)]` becomes
    // `#[allow(dead_code)]` once `$d` is bound to `$` and `h` to the call's `#`.
    let through_an_argument = "macro_rules! outer {\n    ($d:tt) => {\n        \
         macro_rules! inner {\n            ($d h:tt [$d ($d r:tt)*]) => \
         { $d h [allow(dead_code)] fn hidden() {} };\n        }\n    };\n}\n\
         outer!($);\ninner!(#[doc = \"x\"]);\n";
    let through_a_call = "macro_rules! inner {\n    ($d:tt q) => {\n        \
         macro_rules! z {\n            ($d h:tt $d t:tt) => \
         { $d h [allow(dead_code)] fn hidden() {} };\n        }\n    };\n}\n\
         macro_rules! outer {\n    () => { inner!($q); };\n}\nouter!();\nz! { /// x\n}\n";
    let in_a_block = "// one\nfn f() {\n    macro_rules! m { () => {}; }\n}\n";
    let raw = "// one\nm!(r#macro_rules! n { () => {}; });\n";
    let sources = [
        (through_an_argument, 3),
        (through_a_call, 3),
        (in_a_block, 3),
        (raw, 2),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "a nested definition's `$` can come from a fragment, so its transcriber is not read \
         (wi-b5affa)"
    );
    Ok(())
}

#[test]
fn a_symlinked_manifest_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "manifests/x.toml",
        "[package]\nname = \"x\"\n",
    );
    pr.link(Revision::Head, "Cargo.toml", "manifests/x.toml");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("Cargo.toml", 1)]),
        "which files are crate roots must not depend on a link (wi-b5affa)"
    );
    Ok(())
}

#[test]
fn a_symlinked_member_manifest_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "manifests/x.toml",
        "[package]\nname = \"x\"\n",
    );
    pr.link(
        Revision::Head,
        "crates/x/Cargo.toml",
        "../../manifests/x.toml",
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("crates/x/Cargo.toml", 1)]),
        "a manifest is matched by its file name at any depth (#177)"
    );
    Ok(())
}

#[test]
fn a_file_named_only_dot_rs_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/.rs", "fn f() {}\n");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/.rs", 1)]),
        "temper's `*.rs` walk scans it, a target `path` can compile it, and it has no \
         extension to lex it by (#177)"
    );
    Ok(())
}

#[test]
fn a_macro_forwarding_whole_attributes_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "src/lib.rs",
        "macro_rules! m {\n    ($(#[doc = $d:expr])* $v:vis $n:ident) => {\n        \
         $(#[doc = $d])* $v struct $n; impl $n {}\n    };\n}\n",
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "a repetition that ends in `]` cannot leave a `#` behind"
    );
    Ok(())
}

#[test]
fn a_token_whose_boundary_depends_on_the_edition_fails() -> Result<(), Box<dyn Error>> {
    let raw_c = "m!{ cr#\"x\" }\n#[cfg_attr(all(), allow(dead_code))]\nfn f(){}\nm!{ \"# }\n";
    let lifetime = format!("m!{{ 'r#a'\"' }}{HIDDEN}");
    let sources = [
        (raw_c, 1),
        ("// one\nconst C: &core::ffi::CStr = cr\"x\";\n", 2),
        (lifetime.as_str(), 1),
    ];

    let seen = owned_outcomes(&sources)?;

    assert_eq!(
        seen,
        each_blind_at(&sources),
        "edition 2018 lexes these differently from 2021"
    );
    Ok(())
}

#[test]
fn rust_whose_extension_the_scan_does_not_match_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "src/lib.RS", "#[allow(x)]\nfn f() {}\n");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![blind("src/lib.RS", 1)]),
        "temper's walk matches `.rs` exactly, and `[lib] path` can name this file"
    );
    Ok(())
}

#[test]
fn an_attribute_named_by_a_raw_identifier_fails() -> Result<(), Box<dyn Error>> {
    let sources = [
        "// one\n#[r#allow(unused)]\nfn f() {}\n",
        "// one\n#[r#expect(unused, reason = \"r\")]\nfn f() {}\n",
        "// one\n#[cfg_attr(all(), r#allow(unused))]\nfn f() {}\n",
    ];

    let seen = outcomes(&sources)?;

    let expected: Vec<_> = sources
        .iter()
        .map(|source| (*source, (FAILED, vec![blind("src/lib.rs", 2)])))
        .collect();
    assert_eq!(seen, expected, "temper's patterns do not read `r#`");
    Ok(())
}

#[test]
fn an_attribute_built_from_a_macro_fragment_fails() -> Result<(), Box<dyn Error>> {
    let sources = [
        "macro_rules! m {\n    ($a:meta) => { #[$a] fn f() {} };\n}\n",
        "macro_rules! m {\n    ($a:meta) => { #![$a] };\n}\n",
        "macro_rules! m {\n    ($a:tt) => { # $a fn f() {} };\n}\n",
    ];

    let seen = outcomes(&sources)?;

    let expected: Vec<_> = sources
        .iter()
        .map(|source| (*source, (FAILED, vec![blind("src/lib.rs", 2)])))
        .collect();
    assert_eq!(
        seen, expected,
        "`m!(allow(..))` would expand to an attribute no scan reads"
    );
    Ok(())
}
