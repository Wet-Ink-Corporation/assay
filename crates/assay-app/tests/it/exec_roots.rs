//! Exec roots from a parsed manifest (04 §2.2): where build-time code lives, so an in-place
//! edit there is exec surface. Every fail-closed branch has an exact-value test.

use assay_app::exec_roots;
use assay_domain::ExecRoot;

fn path(root: &str) -> ExecRoot {
    ExecRoot::Path(root.to_owned())
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
fn build_script_in_the_root_manifest_makes_everything_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"gen.rs\"\n";

    assert_eq!(
        exec_roots("Cargo.toml", manifest),
        vec![ExecRoot::Everything]
    );
}

#[test]
fn build_true_makes_the_crate_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = true\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![path("crates/x")]
    );
}

#[test]
fn build_false_has_no_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = false\n";

    assert_eq!(exec_roots("crates/x/Cargo.toml", manifest), Vec::new());
}

#[test]
fn default_build_script_siblings_are_left_to_the_syn_scan() {
    // Without a `build` key the manifest cannot say whether `build.rs` exists; the
    // `**/build.rs` glob covers the script and #64's syn scan adds its module closure. This
    // pins that the manifest alone yields no root here.
    let manifest = "[package]\nname = \"x\"\n";

    assert_eq!(exec_roots("crates/x/Cargo.toml", manifest), Vec::new());
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
        vec![path("crates/derive")]
    );
}

#[test]
fn proc_macro_underscore_spelling_is_a_proc_macro() {
    let manifest = "[package]\nname = \"derive\"\n\n[lib]\nproc_macro = true\n";

    assert_eq!(
        exec_roots("crates/derive/Cargo.toml", manifest),
        vec![path("crates/derive")]
    );
}

#[test]
fn proc_macro_crate_type_is_a_proc_macro() {
    let manifest = "[package]\nname = \"derive\"\n\n[lib]\ncrate-type = [\"proc-macro\"]\n";

    assert_eq!(
        exec_roots("crates/derive/Cargo.toml", manifest),
        vec![path("crates/derive")]
    );
}

#[test]
fn proc_macro_lib_path_outside_the_crate_adds_its_directory() {
    let manifest = "[package]\nname = \"derive\"\n\n[lib]\nproc-macro = true\npath = \"../shared/derive_impl.rs\"\n";

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
fn plain_library_has_no_root() {
    let manifest =
        "[package]\nname = \"x\"\n\n[lib]\nproc-macro = false\ncrate-type = [\"rlib\"]\n";

    assert_eq!(exec_roots("crates/x/Cargo.toml", manifest), Vec::new());
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
