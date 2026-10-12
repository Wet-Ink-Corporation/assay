//! Tiering edge cases that an attacker controls (05-security.md T17): custom build scripts and
//! workspace proc-macro crates, non-canonical paths, `include_*!` target spelling, and files
//! that are not regular files.

use assay_domain::{ExecRoot, FileKind, PrClass, RepoGlobs, Tier, tier};

use crate::support::{changes, file, globs};

fn root(path: &str) -> ExecRoot {
    ExecRoot::Path(path.to_owned())
}

#[test]
fn custom_named_build_script_edited_in_place_is_exec_surface() {
    let mut set = changes(vec![file("tools/gen.rs", 1, 1)]);
    set.exec_roots = vec![root("tools/gen.rs")];

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
    set.exec_roots = vec![root("crates/derive")];

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
    set.exec_roots = vec![root("crates/derive")];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(tiering.classes, Vec::new());
}

#[test]
fn everything_root_makes_any_change_exec_surface() {
    let mut set = changes(vec![file("README.md", 1, 0)]);
    set.exec_roots = vec![ExecRoot::Everything];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(
        (tiering.tier, tiering.classes),
        (Tier::Full, vec![PrClass::ExecSurface])
    );
}

#[test]
fn empty_path_root_holds_every_path() {
    let mut set = changes(vec![file("src/lib.rs", 1, 0)]);
    set.exec_roots = vec![root("")];

    let tiering = tier(&RepoGlobs::default(), &set);

    assert_eq!(tiering.classes, vec![PrClass::ExecSurface]);
}

/// The exec surface `exec_roots` give each of `paths`, one change at a time.
fn exec_classes(roots: &[ExecRoot], paths: &[&str]) -> Vec<Vec<PrClass>> {
    paths
        .iter()
        .map(|path| {
            let mut set = changes(vec![file(path, 1, 0)]);
            set.exec_roots = roots.to_vec();
            tier(&RepoGlobs::default(), &set).classes
        })
        .collect()
}

#[test]
fn build_sources_hold_crate_rust_files_outside_target_directories() {
    let roots = [ExecRoot::BuildSources {
        crate_dir: "crates/x".to_owned(),
    }];

    let classes = exec_classes(
        &roots,
        &[
            "crates/x/helper.rs",
            "crates/x/build/codegen.rs",
            "crates/x/src/lib.rs",
            "crates/x/tests/it.rs",
            "crates/x/benches/b.rs",
            "crates/x/examples/e.rs",
            "crates/x/README.md",
            "crates/y/helper.rs",
        ],
    );

    let exec = vec![PrClass::ExecSurface];
    assert_eq!(
        classes,
        vec![
            exec.clone(),
            exec,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new()
        ]
    );
}

#[test]
fn root_build_sources_hold_top_level_files_and_mod_files_only() {
    let roots = [ExecRoot::BuildSources {
        crate_dir: String::new(),
    }];

    let classes = exec_classes(&roots, &["helper.rs", "codegen/mod.rs", "notes.md"]);

    let exec = vec![PrClass::ExecSurface];
    assert_eq!(classes, vec![exec.clone(), exec, Vec::new()]);
}

#[test]
fn nested_root_module_reached_by_build_script_awaits_issue_64() {
    // The root `build.rs` can reach `codegen/other.rs` through `mod codegen;` and `src/` through
    // `include!` or `#[path]`. `BuildSources` does not hold them; #64's syn scan adds that
    // closure to `ChangeSet.exec_roots` (Weigh-In default wi-a29f26). This pins the gap.
    let roots = [ExecRoot::BuildSources {
        crate_dir: String::new(),
    }];

    let classes = exec_classes(&roots, &["codegen/other.rs", "src/lib.rs"]);

    assert_eq!(classes, vec![Vec::<PrClass>::new(); 2]);
}

#[test]
fn build_script_include_of_src_awaits_issue_64() {
    // `crates/x/build.rs` doing `include!("src/cli.rs")` runs `src/cli.rs` at build time, but
    // `BuildSources` leaves `src/` out; #64's syn scan adds the `include!` closure to
    // `ChangeSet.exec_roots` (Weigh-In default wi-a29f26). This pins the gap.
    let roots = [ExecRoot::BuildSources {
        crate_dir: "crates/x".to_owned(),
    }];

    let classes = exec_classes(&roots, &["crates/x/src/cli.rs"]);

    assert_eq!(classes, vec![Vec::<PrClass>::new()]);
}

#[test]
fn non_canonical_root_holds_every_path() {
    let roots = [
        root("./crates/x"),
        ExecRoot::BuildSources {
            crate_dir: "crates/../x".to_owned(),
        },
    ];

    let classes = exec_classes(&roots, &["docs/guide.md"]);

    assert_eq!(classes, vec![vec![PrClass::ExecSurface]]);
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
