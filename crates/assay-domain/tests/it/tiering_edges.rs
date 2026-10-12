//! Tiering edge cases that an attacker controls (05-security.md T17): custom build scripts and
//! workspace proc-macro crates, non-canonical paths, `include_*!` target spelling, and files
//! that are not regular files.

use assay_domain::{FileKind, PrClass, RepoGlobs, Tier, tier};

use crate::support::{changes, file, globs};

#[test]
fn custom_named_build_script_edited_in_place_is_exec_surface() {
    let mut set = changes(vec![file("tools/gen.rs", 1, 1)]);
    set.exec_roots = vec!["tools/gen.rs".to_owned()];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (tiering.tier, tiering.classes, tiering.exec_surface_changed),
        (
            Tier::Full,
            vec![PrClass::ExecSurface],
            vec!["tools/gen.rs".to_owned()]
        )
    );
}

#[test]
fn edit_to_a_workspace_proc_macro_crate_source_is_exec_surface() {
    let mut set = changes(vec![file("crates/derive/src/lib.rs", 1, 1)]);
    set.exec_roots = vec!["crates/derive".to_owned()];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (tiering.classes, tiering.exec_surface_changed),
        (
            vec![PrClass::ExecSurface],
            vec!["crates/derive/src/lib.rs".to_owned()]
        )
    );
}

#[test]
fn exec_root_matches_whole_segments_only() {
    let mut set = changes(vec![file("crates/derive-docs/src/lib.rs", 1, 1)]);
    set.exec_roots = vec!["crates/derive".to_owned()];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(tiering.classes, Vec::new());
}

#[test]
fn non_canonical_paths_are_trust_paths() {
    let paths = [
        "./.github/x",
        "/.github/x",
        "docs/../.github/x",
        "",
        "docs//a.md",
        "\"docs/a.md\"",
        "docs\\a.md",
        "docs/a\u{7}.md",
    ];

    let wrong: Vec<_> = paths
        .iter()
        .filter(|path| {
            let tiering = tier(&RepoGlobs::default(), &changes(vec![file(path, 1, 0)]));
            (tiering.tier, tiering.classes) != (Tier::Full, vec![PrClass::TrustPath])
        })
        .collect();

    assert_eq!(wrong, Vec::<&&str>::new());
}

#[test]
fn include_target_matches_regardless_of_ascii_case() {
    let mut set = changes(vec![file("README.md", 1, 1)]);
    set.include_targets = vec!["readme.MD".to_owned()];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(tiering.tier, Tier::Micro);
}

#[test]
fn gitlink_at_a_docs_path_is_not_trivial() {
    let submodule = file("docs/x.md", 1, 1).with_kind(FileKind::Gitlink);

    let tiering = tier(&RepoGlobs::default(), &changes(vec![submodule]));

    assert_eq!(tiering.tier, Tier::Micro);
}

#[test]
fn symlink_at_a_docs_path_is_not_trivial() {
    let link = file("docs/x.md", 1, 0).with_kind(FileKind::Symlink);

    let tiering = tier(&RepoGlobs::default(), &changes(vec![link]));

    assert_eq!(tiering.tier, Tier::Micro);
}

#[test]
fn symlink_or_gitlink_is_never_excluded() -> Result<(), assay_domain::GlobError> {
    let repo = globs(&[], &[], &["gen/**"], &[])?;
    let files = vec![
        file("gen/link.json", 1, 0).with_kind(FileKind::Symlink),
        file("gen/module", 1, 1).with_kind(FileKind::Gitlink),
    ];

    let filtered = tier(&repo, &changes(files)).filtered;

    assert_eq!((filtered.files, filtered.excluded), (2, Vec::new()));
    Ok(())
}
