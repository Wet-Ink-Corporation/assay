//! Tiering inputs parsed in-process (`docs/architecture/04-pipeline.md` §2.2): base
//! `.assay/globs.toml`, the trust keys of TOML-parsed manifests and the temper baseline ratchet.

use assay_app::{GlobsConfigError, baseline_change, manifest_trust_keys, parse_globs};
use assay_domain::{BaselineChange, GlobError, ManifestKey};

const PACKAGE: &str = r#"
[package]
name = "x"
version = "0.1.0"
edition = "2024"
rust-version = "1.96"

[lib]
doctest = true

[[test]]
name = "it"
harness = true

[lints]
workspace = true

[dependencies]
serde = "1"
"#;

/// `PACKAGE` with `from` replaced by `to`, which must occur.
fn edited(from: &str, to: &str) -> String {
    assert!(PACKAGE.contains(from), "the fixture contains {from:?}");
    PACKAGE.replacen(from, to, 1)
}

#[test]
fn unchanged_manifest_has_no_trust_keys() {
    assert_eq!(
        manifest_trust_keys(Some(PACKAGE), Some(PACKAGE)),
        Vec::new()
    );
}

#[test]
fn dependency_change_is_not_a_trust_key() {
    let head = edited(r#"serde = "1""#, r#"serde = "1.0.200""#);

    assert_eq!(manifest_trust_keys(Some(PACKAGE), Some(&head)), Vec::new());
}

#[test]
fn formatting_only_change_is_not_a_trust_key() {
    let head = edited(
        r#"rust-version = "1.96""#,
        "rust-version   =   '1.96' # pinned",
    );

    assert_eq!(manifest_trust_keys(Some(PACKAGE), Some(&head)), Vec::new());
}

#[test]
fn every_package_trust_key_change_is_found() {
    let cases = [
        (
            edited(
                "[lints]\nworkspace = true",
                "[lints.clippy]\nunwrap_used = \"allow\"",
            ),
            ManifestKey::Lints,
        ),
        (
            edited(r#"rust-version = "1.96""#, r#"rust-version = "1.90""#),
            ManifestKey::RustVersion,
        ),
        (
            edited(r#"edition = "2024""#, r#"edition = "2021""#),
            ManifestKey::Edition,
        ),
        (
            edited("doctest = true", "doctest = false"),
            ManifestKey::TestSwitch,
        ),
        (
            edited("harness = true", "harness = false"),
            ManifestKey::TestSwitch,
        ),
        (
            edited("[lib]", "[lib]\ntest = false"),
            ManifestKey::TestSwitch,
        ),
        (
            edited("[package]", "[package]\nautotests = false"),
            ManifestKey::TestSwitch,
        ),
        (
            edited("[package]", "[package]\nautobenches = false"),
            ManifestKey::TestSwitch,
        ),
        (
            format!("{PACKAGE}\n[patch.crates-io]\nserde = {{ path = \"../serde\" }}\n"),
            ManifestKey::Patch,
        ),
        (
            format!("{PACKAGE}\n[replace]\n\"serde:1.0.0\" = {{ path = \"../serde\" }}\n"),
            ManifestKey::Replace,
        ),
    ];

    let found: Vec<_> = cases
        .iter()
        .map(|(head, _)| manifest_trust_keys(Some(PACKAGE), Some(head)))
        .collect();

    let expected: Vec<_> = cases.iter().map(|(_, key)| vec![*key]).collect();
    assert_eq!(found, expected);
}

#[test]
fn every_workspace_trust_key_change_is_found() {
    let base = r#"
[workspace]
members = ["crates/*"]
exclude = ["fuzz"]

[workspace.package]
edition = "2024"
rust-version = "1.96"

[workspace.lints.rust]
unsafe_code = "forbid"
"#;
    let cases = [
        (
            base.replace(
                r#"members = ["crates/*"]"#,
                r#"members = ["crates/*", "x"]"#,
            ),
            ManifestKey::WorkspaceMembers,
        ),
        (
            base.replace(r#"exclude = ["fuzz"]"#, "exclude = []"),
            ManifestKey::WorkspaceExclude,
        ),
        (
            base.replace(r#"unsafe_code = "forbid""#, r#"unsafe_code = "allow""#),
            ManifestKey::WorkspaceLints,
        ),
        (
            base.replace(r#"rust-version = "1.96""#, r#"rust-version = "1.97""#),
            ManifestKey::RustVersion,
        ),
        (
            base.replace(r#"edition = "2024""#, r#"edition = "2021""#),
            ManifestKey::Edition,
        ),
    ];

    let found: Vec<_> = cases
        .iter()
        .map(|(head, _)| manifest_trust_keys(Some(base), Some(head)))
        .collect();

    let expected: Vec<_> = cases.iter().map(|(_, key)| vec![*key]).collect();
    assert_eq!(found, expected);
}

#[test]
fn several_changed_keys_are_reported_in_vocabulary_order() {
    let head = edited(r#"edition = "2024""#, r#"edition = "2021""#)
        .replace("[lints]\nworkspace = true", "");

    assert_eq!(
        manifest_trust_keys(Some(PACKAGE), Some(&head)),
        vec![ManifestKey::Lints, ManifestKey::Edition]
    );
}

#[test]
fn unparseable_manifest_counts_as_changed() {
    let broken = "[package\nname = ";

    let found = [
        manifest_trust_keys(Some(broken), Some(PACKAGE)),
        manifest_trust_keys(Some(PACKAGE), Some(broken)),
    ];

    assert_eq!(
        found,
        [
            vec![ManifestKey::Unparseable],
            vec![ManifestKey::Unparseable]
        ]
    );
}

#[test]
fn added_manifest_reports_the_trust_keys_it_sets() {
    assert_eq!(
        manifest_trust_keys(None, Some(PACKAGE)),
        vec![
            ManifestKey::Lints,
            ManifestKey::RustVersion,
            ManifestKey::Edition,
            ManifestKey::TestSwitch
        ]
    );
}

#[test]
fn target_edition_change_is_an_edition_change() {
    let head = edited("[lib]", "[lib]\nedition = \"2015\"");

    assert_eq!(
        manifest_trust_keys(Some(PACKAGE), Some(&head)),
        vec![ManifestKey::Edition]
    );
}

#[test]
fn edition_of_a_bin_or_example_target_is_an_edition_change() {
    let heads = [
        format!("{PACKAGE}\n[[bin]]\nname = \"tool\"\nedition = \"2018\"\n"),
        format!("{PACKAGE}\n[[example]]\nname = \"demo\"\nedition = \"2018\"\n"),
    ];

    let found: Vec<_> = heads
        .iter()
        .map(|head| manifest_trust_keys(Some(PACKAGE), Some(head)))
        .collect();

    assert_eq!(found, vec![vec![ManifestKey::Edition]; 2]);
}

#[test]
fn removed_manifest_reports_the_trust_keys_it_had() {
    assert_eq!(
        manifest_trust_keys(Some(PACKAGE), None),
        vec![
            ManifestKey::Lints,
            ManifestKey::RustVersion,
            ManifestKey::Edition,
            ManifestKey::TestSwitch
        ]
    );
}

const BASELINE: &str = r#"{
  "temper_baseline": 1,
  "totals": { "assay-app": { "clippy::too_many_lines": 3, "missing_docs": 1 } }
}"#;

#[test]
fn baseline_with_every_count_down_or_equal_is_lowered() {
    let heads = [
        BASELINE.replace(": 3", ": 2"),
        BASELINE.replace(r#", "missing_docs": 1"#, ""),
        BASELINE.to_owned(),
    ];

    let changes = heads
        .iter()
        .map(|head| baseline_change(Some(BASELINE), Some(head)));

    assert!(
        changes
            .into_iter()
            .all(|change| change == BaselineChange::LoweredOrUnchanged),
        "every head lowers or keeps every count"
    );
}

#[test]
fn baseline_with_a_raised_or_new_count_is_other() {
    let heads = [
        BASELINE.replace(": 3", ": 4"),
        BASELINE.replace(
            r#""missing_docs": 1"#,
            r#""missing_docs": 1, "dead_code": 1"#,
        ),
        BASELINE.replace(r#""assay-app""#, r#""assay-cli": { "x": 1 }, "assay-app""#),
    ];

    let changes: Vec<_> = heads
        .iter()
        .map(|head| baseline_change(Some(BASELINE), Some(head)))
        .collect();

    assert_eq!(changes, vec![BaselineChange::Other; 3]);
}

#[test]
fn baseline_that_does_not_parse_or_is_added_or_removed_is_other() {
    let changes = [
        baseline_change(Some(BASELINE), Some("{")),
        baseline_change(Some(BASELINE), Some(r#"{"totals": {}}"#)),
        baseline_change(
            Some(BASELINE),
            Some(r#"{"temper_baseline": 2, "totals": {}}"#),
        ),
        baseline_change(None, Some(BASELINE)),
        baseline_change(Some(BASELINE), None),
    ];

    assert_eq!(changes, [BaselineChange::Other; 5]);
}

const MAPPED_BASELINE: &str = r#"{
  "temper_baseline": 1,
  "commit": "0123456789abcdef0123456789abcdef01234567",
  "updated": "2026-10-01T00:00:00Z",
  "crates": { "assay-app": "crates/assay-app", "ghost": "crates/ghost" },
  "totals": { "assay-app": { "missing_docs": 3 }, "ghost": { "dead_code": 2 } }
}"#;

#[test]
fn baseline_whose_crates_are_remapped_is_other() {
    let head = MAPPED_BASELINE.replace(
        r#""ghost": "crates/ghost""#,
        r#""ghost": "crates/assay-app""#,
    );

    assert_eq!(
        baseline_change(Some(MAPPED_BASELINE), Some(&head)),
        BaselineChange::Other
    );
}

#[test]
fn baseline_whose_commit_changes_is_other() {
    let head = MAPPED_BASELINE.replace("0123456789abcdef", "fedcba9876543210");

    assert_eq!(
        baseline_change(Some(MAPPED_BASELINE), Some(&head)),
        BaselineChange::Other
    );
}

#[test]
fn baseline_tightened_with_a_new_timestamp_is_lowered() {
    let head = MAPPED_BASELINE
        .replace(r#""missing_docs": 3"#, r#""missing_docs": 2"#)
        .replace("2026-10-01", "2026-10-12");

    assert_eq!(
        baseline_change(Some(MAPPED_BASELINE), Some(&head)),
        BaselineChange::LoweredOrUnchanged
    );
}

#[test]
fn baseline_count_that_is_not_a_count_is_other() {
    let head = BASELINE.replace(": 3", ": -1");

    assert_eq!(
        baseline_change(Some(BASELINE), Some(&head)),
        BaselineChange::Other
    );
}

#[test]
fn globs_toml_adds_to_every_list() -> Result<(), GlobsConfigError> {
    let text = r#"
risk = ["**/billing/**"]
trust = ["crates/**", "schemas/**"]
exclude = ["gen/**"]
docs = ["guides/**/*.md"]
"#;

    let globs = parse_globs(text)?;

    let patterns = |list: &[assay_domain::Glob]| -> Vec<String> {
        list.iter().map(|glob| glob.as_str().to_owned()).collect()
    };
    assert_eq!(
        [
            patterns(&globs.risk),
            patterns(&globs.trust),
            patterns(&globs.exclude),
            patterns(&globs.docs),
        ],
        [
            vec!["**/billing/**".to_owned()],
            vec!["crates/**".to_owned(), "schemas/**".to_owned()],
            vec!["gen/**".to_owned()],
            vec!["guides/**/*.md".to_owned()],
        ]
    );
    Ok(())
}

#[test]
fn empty_globs_toml_adds_nothing() -> Result<(), GlobsConfigError> {
    assert_eq!(parse_globs("")?, assay_domain::RepoGlobs::default());
    Ok(())
}

#[test]
fn globs_toml_refuses_an_unknown_key() {
    assert!(
        matches!(
            parse_globs(r#"remove = ["**/auth/**"]"#),
            Err(GlobsConfigError::Syntax(_))
        ),
        "a key that could remove a glob is refused"
    );
}

#[test]
fn globs_toml_refuses_a_malformed_glob_naming_it() {
    let refused = parse_globs(r#"risk = ["ok/**", "bad?"]"#);

    assert!(
        matches!(
            refused,
            Err(GlobsConfigError::Glob { key: "risk", ref pattern, source: GlobError::Reserved('?') })
                if pattern == "bad?"
        ),
        "got {refused:?}"
    );
}
