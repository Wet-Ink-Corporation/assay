//! Step 4a, the test inventory (`docs/architecture/03-contracts.md` §2.1): test-family
//! attributes and ignores counted per file at base and head; a decrease, a new ignore, a new
//! condition on a test or a module, or a dropped module whose file held tests fails
//! `test-inventory` unless base admits that file's exact change.

use std::error::Error;

use assay_app::{Conclusion, GateOutcome};
use assay_domain::Severity;
use assay_gates::Revision;

use crate::temper_support::{EXCEPTIONS, Pr, Seen, object_id};

const FAILED: GateOutcome = GateOutcome::Concluded(Conclusion::Fail);
const PASSED: GateOutcome = GateOutcome::Concluded(Conclusion::Pass);

const FILE: &str = "crates/x/tests/it.rs";

const TWO_TESTS: &str = "#[test]\nfn a() {}\n\n#[test]\nfn b() {}\n";
const ONE_TEST: &str = "#[test]\nfn a() {}\n";

/// The crate's manifest, so its test targets are crate roots.
const MANIFEST: (&str, &str) = (
    "crates/x/Cargo.toml",
    "[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n",
);

fn inventory(file: &str) -> Seen {
    (
        "test-inventory".to_owned(),
        file.to_owned(),
        1,
        Severity::Blocker,
    )
}

/// A PR whose `FILE` is `base` at base and `head` at head.
fn changed(base: &str, head: &str) -> Result<Pr, Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.both(MANIFEST.0, MANIFEST.1);
    pr.set(Revision::Base, FILE, base);
    pr.set(Revision::Head, FILE, head);
    Ok(pr)
}

#[test]
fn a_removed_test_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = changed(TWO_TESTS, ONE_TEST)?;

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (FAILED, vec![inventory(FILE)]), "2 tests became 1");
    Ok(())
}

#[test]
fn unchanged_and_added_tests_pass() -> Result<(), Box<dyn Error>> {
    for (base, head) in [(TWO_TESTS, TWO_TESTS), (ONE_TEST, TWO_TESTS)] {
        let mut pr = changed(base, head)?;

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (PASSED, Vec::new()), "{base:?} -> {head:?}");
    }
    Ok(())
}

#[test]
fn a_new_ignore_fails() -> Result<(), Box<dyn Error>> {
    for head in [
        "#[test]\n#[ignore]\nfn a() {}\n",
        "#[test]\n#[ignore = \"slow\"]\nfn a() {}\n",
        "#[test]\n#[cfg_attr(miri, ignore)]\nfn a() {}\n",
        "#[test_case(1 ; \"one\" => ignore)]\nfn a(x: u8) {}\n",
    ] {
        let mut pr = changed(ONE_TEST, head)?;

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![inventory(FILE)]), "{head}");
    }
    Ok(())
}

#[test]
fn every_test_family_attribute_counts() -> Result<(), Box<dyn Error>> {
    for base in [
        "#[tokio::test]\nasync fn a() {}\n",
        "#[::core::prelude::v1::test]\nfn a() {}\n",
        "#[r#test]\nfn a() {}\n",
        "#[rstest]\n#[case(1)]\nfn a(#[case] x: u8) {}\n",
        "#[bench]\nfn a(b: &mut Bencher) {}\n",
        "proptest! {\n    #[test]\n    fn a(x in 0..3u8) {}\n}\n",
        "#[wasm_bindgen_test]\nfn a() {}\n",
    ] {
        let mut pr = changed(base, "fn a() {}\n")?;

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![inventory(FILE)]), "{base}");
    }
    Ok(())
}

#[test]
fn a_test_in_a_comment_or_a_string_does_not_count() -> Result<(), Box<dyn Error>> {
    let base = "// #[test]\n/* #[test] */\nconst S: &str = \"#[test]\";\n";
    let mut pr = changed(base, "")?;

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "no test was there");
    Ok(())
}

#[test]
fn a_test_put_under_a_new_condition_fails() -> Result<(), Box<dyn Error>> {
    for head in [
        "#[cfg(any())]\n#[test]\nfn a() {}\n",
        "#[test]\n#[cfg(feature = \"slow\")]\nfn a() {}\n",
        "#[cfg_attr(all(), test)]\nfn a() {}\n",
        "#[cfg_attr(x, cfg(any()))]\n#[test]\nfn a() {}\n",
    ] {
        let mut pr = changed(ONE_TEST, head)?;

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![inventory(FILE)]), "{head}");
    }
    Ok(())
}

#[test]
fn a_new_condition_on_a_module_or_a_file_fails() -> Result<(), Box<dyn Error>> {
    let base = "mod a;\n#[cfg(test)]\nmod tests {}\n";
    for head in [
        "#[cfg(any())]\nmod a;\n#[cfg(test)]\nmod tests {}\n",
        "mod a;\n#[cfg(all(test, any()))]\nmod tests {}\n",
        "#![cfg(any())]\nmod a;\n#[cfg(test)]\nmod tests {}\n",
        "mod a;\n#[cfg(test)]\npub(crate) mod tests {\n    #![cfg(any())]\n}\n",
        "mod a;\n#[cfg(test)]\n#[cfg_attr(x, cfg(any()))]\nmod tests {}\n",
    ] {
        let mut pr = changed(base, head)?;
        pr.both("crates/x/tests/a.rs", "");

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![inventory(FILE)]), "{head}");
    }
    Ok(())
}

#[test]
fn a_new_test_module_under_cfg_test_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = changed(
        "pub fn f() {}\n",
        "pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn f() {}\n}\n",
    )?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "`cfg(test)` holds in every test build"
    );
    Ok(())
}

#[test]
fn a_dropped_module_whose_file_held_tests_fails() -> Result<(), Box<dyn Error>> {
    let main = "crates/x/tests/it/main.rs";
    let mut pr = Pr::new()?;
    pr.both(MANIFEST.0, MANIFEST.1);
    pr.set(Revision::Base, main, "mod a;\nmod b;\n");
    pr.set(Revision::Head, main, "mod b;\n");
    pr.both("crates/x/tests/it/a.rs", ONE_TEST);
    pr.both("crates/x/tests/it/b.rs", ONE_TEST);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(main)]),
        "`a.rs` still holds its test, but nothing compiles it"
    );
    Ok(())
}

#[test]
fn a_dropped_module_without_tests_passes() -> Result<(), Box<dyn Error>> {
    let main = "crates/x/src/lib.rs";
    let mut pr = Pr::new()?;
    pr.both(MANIFEST.0, MANIFEST.1);
    pr.set(Revision::Base, main, "mod a;\n");
    pr.set(Revision::Head, main, "");
    pr.both("crates/x/src/a.rs", "pub fn f() {}\n");

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "no test was dropped");
    Ok(())
}

#[test]
fn a_deleted_test_file_fails_at_its_base_path() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, FILE, ONE_TEST);

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (FAILED, vec![inventory(FILE)]), "1 test became 0");
    Ok(())
}

#[test]
fn a_changed_base_file_that_does_not_lex_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = changed("const S: &str = \"never ends;\n", ONE_TEST)?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(FILE)]),
        "its base tests cannot be counted"
    );
    Ok(())
}

#[test]
fn base_admits_the_exact_change_of_one_file() -> Result<(), Box<dyn Error>> {
    let mut pr = changed(TWO_TESTS, ONE_TEST)?;
    let admitted = format!(
        "[[test_inventory]]\npath = \"{FILE}\"\nbase = \"{}\"\nhead = \"{}\"\nreason = \"merged a duplicate\"\n",
        object_id(TWO_TESTS.as_bytes()),
        object_id(ONE_TEST.as_bytes()),
    );
    pr.set(Revision::Base, EXCEPTIONS, &admitted);

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "the exact pair of blobs");
    Ok(())
}

#[test]
fn base_does_not_admit_another_head() -> Result<(), Box<dyn Error>> {
    let mut pr = changed(TWO_TESTS, "")?;
    let admitted = format!(
        "[[test_inventory]]\npath = \"{FILE}\"\nbase = \"{}\"\nhead = \"{}\"\nreason = \"merged a duplicate\"\n",
        object_id(TWO_TESTS.as_bytes()),
        object_id(ONE_TEST.as_bytes()),
    );
    pr.set(Revision::Base, EXCEPTIONS, &admitted);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(FILE)]),
        "the head blob differs"
    );
    Ok(())
}

#[test]
fn a_deletion_is_admitted_without_a_head_blob() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, FILE, ONE_TEST);
    let admitted = format!(
        "[[test_inventory]]\npath = \"{FILE}\"\nbase = \"{}\"\nreason = \"the crate is gone\"\n",
        object_id(ONE_TEST.as_bytes()),
    );
    pr.set(Revision::Base, EXCEPTIONS, &admitted);

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "base blob, no head");
    Ok(())
}

#[test]
fn a_pathological_nest_of_conditions_is_refused() -> Result<(), Box<dyn Error>> {
    let nest = "#[cfg(a)] x\n#[test] y\n".repeat(3_000);
    let mut pr = changed(&nest, &format!("{nest}// changed\n"))?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(FILE)]),
        "past the work budget the head is not counted, so it fails"
    );
    Ok(())
}

#[test]
fn a_dropped_module_this_check_cannot_place_is_refused() -> Result<(), Box<dyn Error>> {
    let lib = "crates/x/src/lib.rs";
    let far_path = format!(
        "#[path = \"t.rs\"]\n{}mod tests;\n",
        "#[doc = \"x\"]\n".repeat(65)
    );
    for base in [
        "#[path = \"t.rs\"]\nmod tests;\n",
        "fn f() {\n    mod inner;\n}\n",
        "#[cfg_attr(unix, path = \"t.rs\")]\nmod tests;\n",
        far_path.as_str(),
    ] {
        let mut pr = Pr::new()?;
        pr.both(MANIFEST.0, MANIFEST.1);
        pr.set(Revision::Base, lib, base);
        pr.set(Revision::Head, lib, "");
        pr.both("crates/x/src/t.rs", ONE_TEST);

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![inventory(lib)]), "{base}");
    }
    Ok(())
}

/// A conditional test, then an unconditional one.
const CONDITIONAL: &str = "#[cfg(unix)]\n#[test]\nfn a() {}\n\n#[test]\nfn b() {}\n";

#[test]
fn a_deleted_conditional_test_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = changed(CONDITIONAL, "#[test]\nfn b() {}\n")?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(FILE)]),
        "the `cfg(unix)` test is gone"
    );
    Ok(())
}

#[test]
fn a_conditional_test_whose_condition_changes_fails() -> Result<(), Box<dyn Error>> {
    let head = CONDITIONAL.replace("cfg(unix)", "cfg(any())");
    let mut pr = changed(CONDITIONAL, &head)?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(FILE)]),
        "`cfg(any())` never holds"
    );
    Ok(())
}

#[test]
fn tests_under_a_new_condition_on_any_item_fail() -> Result<(), Box<dyn Error>> {
    let calls = "proptest! {\n    #[test]\n    fn a(x in 0..1u8) {}\n}\n";
    let function = "fn outer() {\n    #[test]\n    fn a() {}\n}\n";
    let inline = "mod m {\n    #[test]\n    fn a() {}\n}\n";
    for base in [calls, function, inline] {
        let head = format!("#[cfg(any())]\n{base}");
        let mut pr = changed(base, &head)?;

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![inventory(FILE)]), "{head}");
    }
    Ok(())
}

#[test]
fn a_condition_changed_on_an_enclosing_item_fails() -> Result<(), Box<dyn Error>> {
    let base = "#[cfg(unix)]\nproptest! {\n    #[test]\n    fn a(x in 0..1u8) {}\n}\n";
    let head = base.replace("cfg(unix)", "cfg(any())");
    let mut pr = changed(base, &head)?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(FILE)]),
        "the call's tests are keyed by the condition around them"
    );
    Ok(())
}

#[test]
fn a_module_dropped_above_its_tests_fails() -> Result<(), Box<dyn Error>> {
    let lib = "crates/x/src/lib.rs";
    let mut pr = Pr::new()?;
    pr.both(MANIFEST.0, MANIFEST.1);
    pr.set(Revision::Base, lib, "mod foo;\n");
    pr.set(Revision::Head, lib, "");
    pr.both("crates/x/src/foo.rs", "#[cfg(test)]\nmod tests;\n");
    pr.both("crates/x/src/foo/tests.rs", ONE_TEST);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(lib)]),
        "`foo/tests.rs` still holds its test, two modules down"
    );
    Ok(())
}

#[test]
fn a_module_dropped_whose_file_is_deleted_still_reaches_its_tests() -> Result<(), Box<dyn Error>> {
    let lib = "crates/x/src/lib.rs";
    let foo = "crates/x/src/foo.rs";
    let mut pr = Pr::new()?;
    pr.both(MANIFEST.0, MANIFEST.1);
    pr.set(Revision::Base, lib, "mod foo;\n");
    pr.set(Revision::Head, lib, "");
    pr.set(Revision::Base, foo, "#[cfg(test)]\nmod tests;\n");
    pr.both("crates/x/src/foo/tests.rs", ONE_TEST);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![inventory(foo), inventory(lib)]),
        "`foo/tests.rs` is still in head and compiled by nothing: both drops reach it"
    );
    Ok(())
}
