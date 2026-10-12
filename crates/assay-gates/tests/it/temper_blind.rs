//! What the pinned `temper scan --all` cannot see fails the gate instead (03 §2.1 step 2):
//! `.rs` files under the directories its walk skips or behind a symlinked directory, and
//! attributes it does not read (`cfg_attr` carrying `allow`, `expect` or `path`, and `#[path]`).

use std::error::Error;

use assay_app::{Conclusion, GateOutcome};
use assay_domain::Severity;
use assay_gates::Revision;

use crate::temper_support::{Pr, Seen};

const FAILED: GateOutcome = GateOutcome::Concluded(Conclusion::Fail);
const PASSED: GateOutcome = GateOutcome::Concluded(Conclusion::Pass);

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
    pr.set(Revision::Head, "src/lib.rs", "pub fn f() {}\n");
    pr.link(Revision::Head, "src/alias.rs", "lib.rs");

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "the scan reads through it");
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
