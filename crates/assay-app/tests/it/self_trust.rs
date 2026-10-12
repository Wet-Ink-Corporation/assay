//! The assay repository's own trust paths (02 §7, 05 T16): base `.assay/globs.toml` makes all
//! of `crates/**`, the rubric, the plugin, the schemas, the workflow templates, the release
//! workflow, `.assay/`, `deny.toml` and the golden snapshots trust paths, so changing any of them
//! (an `assay-self` snapshot update included) needs owner approval under rubric rule 4.

use std::error::Error;
use std::fs;
use std::path::PathBuf;

use assay_app::parse_globs;
use assay_domain::{
    BaselineChange, ChangeSet, FileChange, FileKind, Lines, PackageDiff, PrClass, RepoGlobs,
    Tiering, tier,
};

fn repo_globs() -> Result<RepoGlobs, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.assay/globs.toml");
    Ok(parse_globs(&fs::read_to_string(path)?)?)
}

/// The tiering of a PR that changes one line of each of `paths` and nothing else.
fn tiering(globs: &RepoGlobs, paths: &[&str]) -> Tiering {
    let changes = ChangeSet {
        files: paths
            .iter()
            .map(|path| FileChange {
                path: (*path).to_owned(),
                renamed_from: None,
                lines: Lines::Counted {
                    added: 1,
                    removed: 1,
                },
                kind: FileKind::File,
            })
            .collect(),
        manifests: Vec::new(),
        include_targets: Vec::new(),
        exec_roots: Vec::new(),
        packages: PackageDiff::Computed(Vec::new()),
        temper_baseline: BaselineChange::Other,
    };
    tier(globs, &changes)
}

#[test]
fn every_path_02_7_names_is_a_trust_path_of_the_assay_repo() -> Result<(), Box<dyn Error>> {
    let globs = repo_globs()?;
    let paths = [
        "crates/assay-domain/src/decide.rs",
        "crates/assay-forge/README.md",
        "rubric/v1.toml",
        "plugin/prompts/v1/system.md",
        "schemas/decision-1.schema.json",
        "workflows/assay.yml.tmpl",
        ".github/workflows/release.yml",
        ".assay/globs.toml",
        "deny.toml",
    ];

    let unmatched: Vec<&str> = paths
        .into_iter()
        .filter(|path| tiering(&globs, &[path]).trust_paths_changed.is_empty())
        .collect();

    assert_eq!(
        unmatched,
        Vec::<&str>::new(),
        "each is a trust path on its own"
    );
    Ok(())
}

#[test]
fn a_golden_snapshot_update_is_a_trust_path_change() -> Result<(), Box<dyn Error>> {
    let globs = repo_globs()?;
    let snapshot = "corpus/golden/rule-9-success/decision.json";

    let tiering = tiering(&globs, &[snapshot]);

    assert_eq!(
        (tiering.trust_paths_changed, tiering.classes),
        (vec![snapshot.to_owned()], vec![PrClass::TrustPath]),
        "a snapshot update needs owner approval (rule 4) whatever caused it"
    );
    Ok(())
}

#[test]
fn documentation_outside_the_trust_paths_is_not_one() -> Result<(), Box<dyn Error>> {
    let globs = repo_globs()?;

    let tiering = tiering(
        &globs,
        &[
            "docs/architecture/02-architecture.md",
            "corpus/dev/README.md",
        ],
    );

    assert_eq!(
        tiering.trust_paths_changed,
        Vec::<String>::new(),
        "the additions are scoped, not everything"
    );
    Ok(())
}
