//! Exec roots from a parsed manifest (04 §2.2): custom build scripts and proc-macro crates,
//! whose in-place edits are exec surface.

use assay_app::exec_roots;

#[test]
fn custom_build_script_is_a_root_relative_to_the_crate() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"tools/gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec!["crates/x/tools/gen.rs".to_owned()]
    );
}

#[test]
fn custom_build_script_path_is_normalised() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"./../shared/gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec!["crates/shared/gen.rs".to_owned()]
    );
}

#[test]
fn build_script_in_the_root_manifest_is_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"gen.rs\"\n";

    assert_eq!(
        exec_roots("Cargo.toml", manifest),
        vec!["gen.rs".to_owned()]
    );
}

#[test]
fn proc_macro_crate_directory_is_a_root() {
    let manifest = "[package]\nname = \"derive\"\n\n[lib]\nproc-macro = true\n";

    assert_eq!(
        exec_roots("crates/derive/Cargo.toml", manifest),
        vec!["crates/derive".to_owned()]
    );
}

#[test]
fn plain_library_has_no_root() {
    let manifest = "[package]\nname = \"x\"\n\n[lib]\nproc-macro = false\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        Vec::<String>::new()
    );
}

#[test]
fn unparseable_manifest_makes_its_whole_crate_a_root() {
    assert_eq!(
        exec_roots("crates/x/Cargo.toml", "[package\n"),
        vec!["crates/x".to_owned()]
    );
}

#[test]
fn build_script_outside_the_repository_makes_everything_a_root() {
    let manifest = "[package]\nname = \"x\"\nbuild = \"../../../gen.rs\"\n";

    assert_eq!(
        exec_roots("crates/x/Cargo.toml", manifest),
        vec![String::new()]
    );
}
