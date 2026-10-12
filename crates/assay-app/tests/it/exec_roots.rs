//! Exec roots from a parsed manifest (04 §2.2): where build-time code lives, so an in-place
//! edit there is exec surface. Every fail-closed branch has an exact-value test.

use assay_app::{exec_roots, manifest_exec_roots};
use assay_domain::{
    BaselineChange, ChangeSet, ExecRoot, FileChange, Lines, PackageDiff, PrClass, RepoGlobs, Tier,
    tier,
};

fn path(root: &str) -> ExecRoot {
    ExecRoot::Path(root.to_owned())
}

fn sources(crate_dir: &str) -> ExecRoot {
    ExecRoot::BuildSources {
        crate_dir: crate_dir.to_owned(),
    }
}

/// The tiering of a one-line change to each of `files` under `roots`.
fn tier_with(roots: Vec<ExecRoot>, files: &[&str]) -> (Tier, Vec<PrClass>) {
    let set = ChangeSet {
        files: files
            .iter()
            .map(|file| {
                FileChange::new(
                    *file,
                    Lines::Counted {
                        added: 1,
                        removed: 0,
                    },
                )
            })
            .collect(),
        manifests: Vec::new(),
        include_targets: Vec::new(),
        exec_roots: roots,
        packages: PackageDiff::Computed(Vec::new()),
        temper_baseline: BaselineChange::Other,
    };
    let tiering = tier(&RepoGlobs::default(), &set);
    (tiering.tier, tiering.classes)
}

#[test]
fn custom_build_script_makes_its_directory_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"tools/gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![path("crates/x/tools")]
    );
}

#[test]
fn legacy_project_build_script_makes_its_directory_a_root() {
    let manifest = "[project]\nname = \"x\"\nbuild = \"tools/gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![path("crates/x/tools")]
    );
}

#[test]
fn custom_build_script_beside_the_manifest_makes_the_crate_a_root() {
    // `gen.rs` may pull in siblings through `mod`, `#[path]` or `include!`.
    let manifest = "[package]\nname = \"x\"\nbuild = \"gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![path("crates/x")]
    );
}

#[test]
fn custom_build_script_path_is_normalised() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"./../shared/gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![path("crates/shared")]
    );
}

#[test]
fn custom_build_script_in_the_root_manifest_makes_everything_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"gen.rs\"\n";

    assert_eq!(
        exec_roots("Cargo.toml", manifest),
        vec![ExecRoot::Everything]
    );
}

#[test]
fn default_build_true_and_build_rs_spellings_agree() {
    let manifests = [
        "[package]\nname = \"x\"\n",
        "[package]\nname = \"x\"\nbuild = true\n",
        "[package]\nname = \"x\"\nbuild = \"build.rs\"\n",
        "[package]\nname = \"x\"\nbuild = \"./build.rs\"\n",
    ];

    let roots: Vec<_> = manifests
        .iter()
        .map(|manifest| exec_roots("crates/x/Cargo.toml", manifest))
        .collect();

    assert_eq!(roots, vec![vec![sources("crates/x")]; 4]);
}

#[test]
fn default_build_script_sibling_module_is_exec_surface() {
    let roots = exec_roots("crates/x/Cargo.toml", "[package]\nname = \"x\"\n");

    assert_eq!(
        tier_with(roots, &["crates/x/helper.rs"]),
        (Tier::Full, vec![PrClass::ExecSurface])
    );
}

#[test]
fn root_package_gets_root_build_sources() {
    assert_eq!(
        exec_roots("Cargo.toml", "[package]\nname = \"x\"\n"),
        vec![sources("")]
    );
}

#[test]
fn build_false_has_no_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = false\n";

    assert_eq!(exec_roots("crates/x/Cargo.toml", manifest), Vec::new());
}

#[test]
fn virtual_workspace_manifest_has_no_root() {
    let manifest = "[workspace]\nmembers = [\"crates/*\"]\n";

    assert_eq!(exec_roots("Cargo.toml", manifest), Vec::new());
}

#[test]
fn build_value_of_another_type_makes_the_crate_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = [\"a.rs\", \"b.rs\"]\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![path("crates/x")]
    );
}

#[test]
fn absolute_build_path_makes_everything_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"/tmp/gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![ExecRoot::Everything]
    );
}

#[test]
fn build_path_with_a_backslash_makes_everything_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = 'tools\\gen.rs'\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![ExecRoot::Everything]
    );
}

#[test]
fn build_script_outside_the_repository_makes_everything_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"../../../gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![ExecRoot::Everything]
    );
}

#[test]
fn proc_macro_crate_directory_is_a_root() {
    let manifest = "[package]\nname = \"derive\"\n\n[lib]\nproc-macro = true\n";

    assert_eq!(
        exec_roots("crates/derive/Cargo.toml", manifest),
        vec![sources("crates/derive"), path("crates/derive")]
    );
}

#[test]
fn proc_macro_underscore_spelling_is_a_proc_macro() {
    let manifest = "[package]\nname = \"derive\"\nbuild = false\n\n[lib]\nproc_macro = true\n";

    assert_eq!(
        exec_roots("crates/derive/Cargo.toml", manifest),
        vec![path("crates/derive")]
    );
}

#[test]
fn proc_macro_crate_type_is_a_proc_macro() {
    let manifest =
        "[package]\nname = \"derive\"\nbuild = false\n\n[lib]\ncrate-type = [\"proc-macro\"]\n";

    assert_eq!(
        exec_roots("crates/derive/Cargo.toml", manifest),
        vec![path("crates/derive")]
    );
}

#[test]
fn proc_macro_lib_path_outside_the_crate_adds_its_directory() {
    let manifest = "[package]\nname = \"derive\"\nbuild = false\n\n[lib]\nproc-macro = true\npath = \"../shared/derive_impl.rs\"\n";

    assert_eq!(
        exec_roots("crates/derive/Cargo.toml", manifest),
        vec![
            path("crates/derive"),
            path("crates/shared/derive_impl.rs"),
            path("crates/shared")
        ]
    );
}

#[test]
fn proc_macro_lib_path_that_cannot_be_placed_makes_everything_a_root() {
    let lib_paths = [
        "/abs/derive.rs",
        "shared\\\\derive.rs",
        "../../../derive.rs",
    ];

    let roots: Vec<_> = lib_paths
        .iter()
        .map(|lib_path| {
            let manifest = format!(
                "[package]\nname = \"derive\"\nbuild = false\n\n[lib]\nproc-macro = true\npath = \"{lib_path}\"\n"
            );
            exec_roots("crates/derive/Cargo.toml", &manifest)
        })
        .collect();

    assert_eq!(
        roots,
        vec![vec![path("crates/derive"), ExecRoot::Everything]; 3]
    );
}

#[test]
fn plain_library_has_only_build_sources() {
    let manifest =
        "[package]\nname = \"x\"\n\n[lib]\nproc-macro = false\ncrate-type = [\"rlib\"]\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![sources("crates/x")]
    );
}

#[test]
fn unparseable_manifest_makes_its_whole_crate_a_root() {
    assert_eq!(
        exec_roots("crates/x/Cargo.toml", "[package\n"),
        vec![path("crates/x")]
    );
}

#[test]
fn unparseable_root_manifest_makes_everything_a_root() {
    assert_eq!(
        exec_roots("Cargo.toml", "[package\n"),
        vec![ExecRoot::Everything]
    );
}

#[test]
fn unchanged_exec_roots_do_not_make_the_manifest_a_root() {
    let manifest = "[package]\nname = \"x\"\n";

    assert_eq!(
        manifest_exec_roots("crates/x/Cargo.toml", Some(manifest), Some(manifest)),
        vec![sources("crates/x")]
    );
}

#[test]
fn manifest_only_change_into_build_time_code_is_never_micro() {
    let base = "[package]\nname = \"x\"\nbuild = false\n";
    let heads = [
        "[package]\nname = \"x\"\nbuild = \"tools/gen.rs\"\n",
        "[package]\nname = \"x\"\nbuild = false\n\n[lib]\nproc-macro = true\n",
    ];

    let tiers: Vec<_> = heads
        .iter()
        .map(|head| {
            let roots = manifest_exec_roots("crates/x/Cargo.toml", Some(base), Some(head));
            tier_with(roots, &["crates/x/Cargo.toml"])
        })
        .collect();

    assert_eq!(tiers, vec![(Tier::Full, vec![PrClass::ExecSurface]); 2]);
}
