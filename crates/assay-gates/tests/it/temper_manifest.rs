//! Steps 4b-4d (`docs/architecture/03-contracts.md` §2.1): manifest switches, a weakened
//! lints table and lint inheritance, from `Cargo.toml` parsed in-process at base and head.

use std::error::Error;

use assay_app::GateJobOutcome;
use assay_domain::{GateConclusion, Severity};
use assay_gates::Revision;

use crate::temper_support::{EXCEPTIONS, Pr, Seen, object_id};

const FAILED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Fail);
const PASSED: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Pass);

const ROOT: &str = "Cargo.toml";
const MEMBER: &str = "crates/x/Cargo.toml";

/// The workspace manifest every fixture starts from.
const WORKSPACE: &str = "[workspace]
members = [\"crates/*\"]
exclude = [\"fuzz\"]

[workspace.package]
edition = \"2024\"
rust-version = \"1.96\"

[workspace.lints.rust]
unsafe_code = \"forbid\"

[workspace.lints.clippy]
all = { level = \"deny\", priority = -1 }
unwrap_used = \"deny\"
";

/// The member manifest every fixture starts from.
const CRATE: &str = "[package]
name = \"x\"
edition.workspace = true
rust-version.workspace = true

[lints]
workspace = true
";

fn seen(rule: &str, file: &str) -> Seen {
    (rule.to_owned(), file.to_owned(), 1, Severity::Blocker)
}

/// A PR with the workspace and its member at base, and `root` and `member` at head.
fn pr(root: &str, member: &str) -> Result<Pr, Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Base, ROOT, WORKSPACE);
    pr.set(Revision::Base, MEMBER, CRATE);
    pr.set(Revision::Head, ROOT, root);
    pr.set(Revision::Head, MEMBER, member);
    Ok(pr)
}

/// `CRATE` with `line` added to its `[package]` table.
fn package_with(line: &str) -> String {
    CRATE.replacen("name = \"x\"\n", &format!("name = \"x\"\n{line}\n"), 1)
}

#[test]
fn an_unchanged_workspace_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = pr(WORKSPACE, CRATE)?;

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "nothing changed");
    Ok(())
}

#[test]
fn each_member_switch_fails() -> Result<(), Box<dyn Error>> {
    for member in [
        format!("{CRATE}\n[lib]\ntest = false\n"),
        format!("{CRATE}\n[[test]]\nname = \"it\"\ntest = false\n"),
        package_with("autotests = false"),
        format!("{CRATE}\n[lib]\ndoctest = false\n"),
        format!("{CRATE}\n[[test]]\nname = \"it\"\nharness = false\n"),
        CRATE.replace("rust-version.workspace = true", "rust-version = \"1.80\""),
        CRATE.replace("edition.workspace = true", "edition = \"2021\""),
        format!("{CRATE}\n[[bin]]\nname = \"x\"\nedition = \"2021\"\n"),
        format!("{CRATE}\n[[test]]\nname = \"it\"\nrequired-features = [\"slow\"]\n"),
        format!("{CRATE}\n[[test]]\nname = \"it\"\npath = \"tests/empty.rs\"\n"),
        package_with("autobins = false"),
        format!("{CRATE}\n[patch.crates-io]\nserde = {{ path = \"../serde\" }}\n"),
    ] {
        let mut pr = pr(WORKSPACE, &member)?;

        let outcome = pr.outcome()?;

        assert_eq!(
            outcome,
            (FAILED, vec![seen("manifest-switch", MEMBER)]),
            "{member}"
        );
    }
    Ok(())
}

#[test]
fn each_workspace_switch_fails() -> Result<(), Box<dyn Error>> {
    for root in [
        format!("{WORKSPACE}\n[patch.crates-io]\nserde = {{ path = \"../serde\" }}\n"),
        format!("{WORKSPACE}\n[replace]\n\"serde:1.0.0\" = {{ path = \"../serde\" }}\n"),
        WORKSPACE.replace("exclude = [\"fuzz\"]", "exclude = [\"fuzz\", \"crates/x\"]"),
        WORKSPACE.replace("rust-version = \"1.96\"", "rust-version = \"1.80\""),
        WORKSPACE.replace("edition = \"2024\"", "edition = \"2021\""),
    ] {
        let mut pr = pr(&root, CRATE)?;

        let outcome = pr.outcome()?;

        assert_eq!(
            outcome,
            (FAILED, vec![seen("manifest-switch", ROOT)]),
            "{root}"
        );
    }
    Ok(())
}

#[test]
fn a_new_crate_that_inherits_its_edition_passes() -> Result<(), Box<dyn Error>> {
    let mut pr = pr(WORKSPACE, CRATE)?;
    pr.set(
        Revision::Head,
        "crates/y/Cargo.toml",
        &CRATE.replace("\"x\"", "\"y\""),
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "`edition.workspace = true` is the workspace's edition"
    );
    Ok(())
}

#[test]
fn a_new_crate_with_its_own_edition_fails() -> Result<(), Box<dyn Error>> {
    let manifest = "crates/y/Cargo.toml";
    let mut pr = pr(WORKSPACE, CRATE)?;
    pr.set(
        Revision::Head,
        manifest,
        &CRATE
            .replace("\"x\"", "\"y\"")
            .replace("edition.workspace = true", "edition = \"2015\""),
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![seen("manifest-switch", manifest)]),
        "an edition the workspace does not set"
    );
    Ok(())
}

#[test]
fn base_admits_the_exact_switch_of_one_manifest() -> Result<(), Box<dyn Error>> {
    let member = format!("{CRATE}\n[lib]\ndoctest = false\n");
    let mut pr = pr(WORKSPACE, &member)?;
    let admitted = format!(
        "[[manifest_switch]]\npath = \"{MEMBER}\"\nbase = \"{}\"\nhead = \"{}\"\nreason = \"no doc examples\"\n",
        object_id(CRATE.as_bytes()),
        object_id(member.as_bytes()),
    );
    pr.set(Revision::Base, EXCEPTIONS, &admitted);

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "the exact pair of blobs");
    Ok(())
}

#[test]
fn a_weakened_workspace_lints_table_fails() -> Result<(), Box<dyn Error>> {
    for root in [
        WORKSPACE.replace("unwrap_used = \"deny\"", "unwrap_used = \"warn\""),
        WORKSPACE.replace("unwrap_used = \"deny\"\n", ""),
        WORKSPACE.replace("priority = -1", "priority = 1"),
        WORKSPACE.replace("level = \"deny\", priority", "level = \"warn\", priority"),
        WORKSPACE.replace("unsafe_code = \"forbid\"", "unsafe_code = \"forbidden\""),
        format!("{WORKSPACE}expect_used = \"allow\"\n"),
        format!("{WORKSPACE}expect_used = \"warn\"\n"),
        WORKSPACE.replace("[workspace.lints.rust]\nunsafe_code = \"forbid\"\n", ""),
    ] {
        let mut pr = pr(&root, CRATE)?;

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (FAILED, vec![seen("lints-table", ROOT)]), "{root}");
    }
    Ok(())
}

#[test]
fn a_strengthened_lints_table_passes() -> Result<(), Box<dyn Error>> {
    for root in [
        WORKSPACE.replace("unwrap_used = \"deny\"", "unwrap_used = \"forbid\""),
        WORKSPACE.replace(
            "unwrap_used = \"deny\"",
            "unwrap_used = { level = \"deny\" }",
        ),
        format!("{WORKSPACE}expect_used = \"deny\"\n"),
        format!("{WORKSPACE}\n[workspace.lints.rustdoc]\nbroken_intra_doc_links = \"forbid\"\n"),
    ] {
        let mut pr = pr(&root, CRATE)?;

        let outcome = pr.outcome()?;

        assert_eq!(outcome, (PASSED, Vec::new()), "{root}");
    }
    Ok(())
}

#[test]
fn removing_the_workspace_manifest_weakens_its_lints() -> Result<(), Box<dyn Error>> {
    let mut pr = pr(WORKSPACE, CRATE)?;
    pr.remove(Revision::Head, ROOT);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![seen("manifest-switch", ROOT), seen("lints-table", ROOT)]
        ),
        "every workspace lint is gone, and every switch with them (#64 review F4)"
    );
    Ok(())
}

#[test]
fn a_member_that_stops_inheriting_fails_twice() -> Result<(), Box<dyn Error>> {
    let member = CRATE.replace(
        "[lints]\nworkspace = true\n",
        "[lints.clippy]\nall = \"deny\"\n",
    );
    let mut pr = pr(WORKSPACE, &member)?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (
            FAILED,
            vec![
                seen("lints-table", MEMBER),
                seen("lint-inheritance", MEMBER)
            ]
        ),
        "its own table replaces the workspace's, and it no longer inherits"
    );
    Ok(())
}

#[test]
fn a_new_member_without_inherited_lints_fails() -> Result<(), Box<dyn Error>> {
    let manifest = "crates/y/Cargo.toml";
    let mut pr = pr(WORKSPACE, CRATE)?;
    pr.set(Revision::Head, manifest, "[package]\nname = \"y\"\n");

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![seen("lint-inheritance", manifest)]),
        "every package outside `exclude` inherits the workspace lints"
    );
    Ok(())
}

#[test]
fn an_excluded_package_and_a_virtual_manifest_need_not_inherit() -> Result<(), Box<dyn Error>> {
    let mut pr = pr(WORKSPACE, CRATE)?;
    pr.set(
        Revision::Head,
        "fuzz/Cargo.toml",
        "[package]\nname = \"fuzz\"\n",
    );
    pr.set(
        Revision::Head,
        "fuzz/sub/Cargo.toml",
        "[package]\nname = \"sub\"\n",
    );

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, Vec::new()),
        "`fuzz` is excluded; the root is a virtual manifest"
    );
    Ok(())
}

#[test]
fn a_root_package_must_inherit_too() -> Result<(), Box<dyn Error>> {
    let root = format!("{WORKSPACE}\n[package]\nname = \"root\"\n");
    let mut pr = pr(&root, CRATE)?;
    pr.set(Revision::Base, ROOT, &root);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![seen("lint-inheritance", ROOT)]),
        "a root package is a member"
    );
    Ok(())
}

#[test]
fn a_manifest_that_does_not_parse_fails() -> Result<(), Box<dyn Error>> {
    let mut pr = pr(WORKSPACE, "[package\nname = \"x\"\n")?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![seen("manifest-unreadable", MEMBER)]),
        "nothing in it can be compared"
    );
    Ok(())
}

#[test]
fn a_workspace_membership_change_fails() -> Result<(), Box<dyn Error>> {
    for (root, member, changed) in [
        (
            WORKSPACE.replace("members = [\"crates/*\"]\n", ""),
            CRATE.to_owned(),
            ROOT,
        ),
        (
            WORKSPACE.replace("members = [\"crates/*\"]", "members = [\"crates/y\"]"),
            CRATE.to_owned(),
            ROOT,
        ),
        (
            WORKSPACE.replace(
                "exclude = [\"fuzz\"]",
                "exclude = [\"fuzz\"]\ndefault-members = [\"crates/y\"]",
            ),
            CRATE.to_owned(),
            ROOT,
        ),
        (
            WORKSPACE.to_owned(),
            package_with("workspace = \"../..\""),
            MEMBER,
        ),
        (
            WORKSPACE.to_owned(),
            format!("{CRATE}\n[workspace]\n"),
            MEMBER,
        ),
    ] {
        let mut pr = pr(&root, &member)?;

        let outcome = pr.outcome()?;

        assert_eq!(
            outcome,
            (FAILED, vec![seen("manifest-switch", changed)]),
            "{root}\n{member}"
        );
    }
    Ok(())
}

#[test]
fn a_removed_path_dependency_fails() -> Result<(), Box<dyn Error>> {
    let root_dependency = "\n[workspace.dependencies]\nhelper = { path = \"crates/helper\" }\n";
    for (table, root) in [
        ("[dependencies]", WORKSPACE.to_owned()),
        ("[dev-dependencies]", WORKSPACE.to_owned()),
        ("[build-dependencies]", WORKSPACE.to_owned()),
        (
            "[target.'cfg(unix)'.dev-dependencies]",
            WORKSPACE.to_owned(),
        ),
        ("", format!("{WORKSPACE}{root_dependency}")),
    ] {
        let mut pr = pr(WORKSPACE, CRATE)?;
        let changed = if table.is_empty() {
            pr.set(Revision::Base, ROOT, &root);
            ROOT
        } else {
            let member = format!("{CRATE}\n{table}\nhelper = {{ path = \"../helper\" }}\n");
            pr.set(Revision::Base, MEMBER, &member);
            MEMBER
        };

        let outcome = pr.outcome()?;

        assert_eq!(
            outcome,
            (FAILED, vec![seen("manifest-switch", changed)]),
            "the helper and its tests leave the graph: {table}"
        );
    }
    Ok(())
}

#[test]
fn a_removed_inherited_path_dependency_fails() -> Result<(), Box<dyn Error>> {
    let path_root =
        format!("{WORKSPACE}\n[workspace.dependencies]\nhelper = {{ path = \"crates/helper\" }}\n");
    let registry_root = format!("{WORKSPACE}\n[workspace.dependencies]\nhelper = \"1\"\n");
    let member = format!("{CRATE}\n[dependencies]\nhelper = {{ workspace = true }}\n");
    for (root, expected) in [
        (path_root, vec![seen("manifest-switch", MEMBER)]),
        (WORKSPACE.to_owned(), vec![seen("manifest-switch", MEMBER)]),
        (registry_root, Vec::new()),
    ] {
        let mut pr = pr(&root, CRATE)?;
        pr.set(Revision::Base, ROOT, &root);
        pr.set(Revision::Base, MEMBER, &member);
        let outcome_expected = if expected.is_empty() { PASSED } else { FAILED };

        let outcome = pr.outcome()?;

        assert_eq!(
            outcome,
            (outcome_expected, expected),
            "a path the workspace names, or one it cannot resolve, leaves the graph: {root}"
        );
    }
    Ok(())
}

#[test]
fn an_inherited_dependency_the_root_does_not_own_is_unresolved() -> Result<(), Box<dyn Error>> {
    let registry = "\n[workspace.dependencies]\nhelper = \"1\"\n";
    let excluding = format!(
        "{}{registry}",
        WORKSPACE.replace("exclude = [\"fuzz\"]", "exclude = [\"fuzz\", \"crates/x\"]")
    );
    let owning = format!("{WORKSPACE}{registry}");
    for (root, own_workspace) in [(excluding, ""), (owning, "\n[workspace]\n")] {
        let head = format!("{CRATE}{own_workspace}");
        let base = format!("{head}\n[dependencies]\nhelper = {{ workspace = true }}\n");
        let mut pr = pr(&root, &head)?;
        pr.set(Revision::Base, ROOT, &root);
        pr.set(Revision::Base, MEMBER, &base);

        let outcome = pr.outcome()?;

        assert_eq!(
            outcome,
            (FAILED, vec![seen("manifest-switch", MEMBER)]),
            "an excluded crate or a workspace of its own does not inherit the root's: {root}"
        );
    }
    Ok(())
}

#[test]
fn an_added_path_dependency_passes() -> Result<(), Box<dyn Error>> {
    let member = format!("{CRATE}\n[dependencies]\nhelper = {{ path = \"../helper\" }}\n");
    let mut pr = pr(WORKSPACE, &member)?;

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "nothing left the graph");
    Ok(())
}

#[test]
fn a_deleted_package_manifest_fails_whatever_it_set() -> Result<(), Box<dyn Error>> {
    let bare = "[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n";
    let mut pr = pr(WORKSPACE, bare)?;
    pr.set(Revision::Base, MEMBER, bare);
    pr.remove(Revision::Head, MEMBER);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![seen("manifest-switch", MEMBER)]),
        "the package and its tests are gone"
    );
    Ok(())
}

#[test]
fn a_deleted_manifest_removes_its_switches() -> Result<(), Box<dyn Error>> {
    let member = format!("{CRATE}\n[lib]\nharness = false\n");
    let mut pr = pr(WORKSPACE, &member)?;
    pr.set(Revision::Base, MEMBER, &member);
    pr.remove(Revision::Head, MEMBER);

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec![seen("manifest-switch", MEMBER)]),
        "every switch it set is gone with it"
    );
    Ok(())
}
