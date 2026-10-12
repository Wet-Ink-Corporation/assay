//! What a revision's Rust reads at compile time, for tiering (`docs/architecture/04-pipeline.md`
//! §2.2 and §2.3): the targets of the include macros and `#[path]`, and the `include!`,
//! `#[path]` and `mod` closure of every build script and proc-macro lib as exec roots.

use std::error::Error;

use assay_domain::{
    BaselineChange, ChangeSet, ExecRoot, FileChange, Lines, PackageDiff, PrClass, RepoGlobs, tier,
};
use assay_gates::{CompileTimeReach, Revision, compile_time_reach};

use crate::temper_support::Pr;

/// A package manifest with `extra` appended.
fn package(extra: &str) -> String {
    format!("[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n{extra}")
}

/// The head's reach with `files` at head.
fn reach(files: &[(&str, &str)]) -> Result<CompileTimeReach, Box<dyn Error>> {
    let mut pr = Pr::new()?;
    for (path, text) in files {
        pr.set(Revision::Head, path, text);
    }
    Ok(compile_time_reach(&pr.revisions, Revision::Head)?)
}

/// `ExecRoot::Path` for each of `paths`.
fn paths(paths: &[&str]) -> Vec<ExecRoot> {
    paths
        .iter()
        .map(|path| ExecRoot::Path((*path).to_owned()))
        .collect()
}

/// The classes tiering gives a PR that changes only `path`, with `roots`.
fn classes_of(roots: &[ExecRoot], path: &str) -> Vec<PrClass> {
    let set = ChangeSet {
        files: vec![FileChange::new(
            path,
            Lines::Counted {
                added: 1,
                removed: 0,
            },
        )],
        manifests: Vec::new(),
        include_targets: Vec::new(),
        exec_roots: roots.to_vec(),
        packages: PackageDiff::Computed(Vec::new()),
        temper_baseline: BaselineChange::Other,
    };
    tier(&RepoGlobs::default(), &set).classes
}

/// The classes tiering gives each of `changed`, each changed alone, with `roots`.
fn classes(roots: &[ExecRoot], changed: &[&str]) -> Vec<Vec<PrClass>> {
    changed.iter().map(|path| classes_of(roots, path)).collect()
}

#[test]
fn data_includes_and_path_attributes_are_include_targets() -> Result<(), Box<dyn Error>> {
    let lib = "#![doc = include_str!(\"../README.md\")]\n\
               #[path = \"imp/unix.rs\"]\n\
               mod sys;\n\
               fn f() {\n    let b = include_bytes!(r#\"data/x.bin\"#);\n}\n";

    let found = reach(&[
        ("crates/x/Cargo.toml", &package("")),
        ("crates/x/src/lib.rs", lib),
    ])?;

    assert_eq!(
        found,
        CompileTimeReach {
            include_targets: vec![
                "crates/x/README.md".to_owned(),
                "crates/x/src/data/x.bin".to_owned(),
                "crates/x/src/imp/unix.rs".to_owned(),
            ],
            exec_roots: Vec::new(),
        },
        "each resolved from the including file's directory"
    );
    Ok(())
}

#[test]
fn a_target_that_cannot_be_placed_matches_every_path() -> Result<(), Box<dyn Error>> {
    for source in [
        "const S: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/x\"));",
        "const S: &str = include_str!(\"../../../../outside.md\");",
        "const S: &str = include_str!(\"/etc/passwd\");",
        "const S: &str = include_str!(\"a\\x2emd\");",
        "const S: &[u8] = include_bytes!(b\"x\");",
        "macro_rules! m { () => { include_str!(\"x.md\") }; }",
        "use std::include_str as text;",
        "#[cfg_attr(unix, path = \"u.rs\")]\nmod u;",
    ] {
        let found = reach(&[("crates/x/src/lib.rs", source)])?;

        assert_eq!(
            found.include_targets,
            [String::new()],
            "an empty target is not canonical, so it matches every path: {source}"
        );
    }
    Ok(())
}

#[test]
fn a_build_script_include_of_src_is_an_exec_root() -> Result<(), Box<dyn Error>> {
    let found = reach(&[
        ("crates/x/Cargo.toml", &package("")),
        (
            "crates/x/build.rs",
            "include!(\"src/cli.rs\");\nfn main() {}\n",
        ),
        ("crates/x/src/cli.rs", "pub fn cli() {}\n"),
    ])?;

    assert_eq!(
        classes(
            &found.exec_roots,
            &["crates/x/src/cli.rs", "crates/x/src/lib.rs"]
        ),
        vec![vec![PrClass::ExecSurface], Vec::new()],
        "the build script runs `src/cli.rs`; it does not run `src/lib.rs`"
    );
    Ok(())
}

#[test]
fn a_root_build_script_module_reaches_its_nested_files() -> Result<(), Box<dyn Error>> {
    let found = reach(&[
        ("Cargo.toml", &package("")),
        ("build.rs", "mod codegen;\nfn main() {}\n"),
        ("codegen.rs", "mod other;\n"),
        ("codegen/other.rs", "pub fn other() {}\n"),
    ])?;

    assert_eq!(
        found.exec_roots,
        paths(&[
            "build.rs",
            "codegen.rs",
            "codegen/mod.rs",
            "codegen/other.rs",
            "codegen/other/mod.rs",
            "other.rs",
            "other/mod.rs",
        ]),
        "a module's `mod` is read in both places it can be"
    );
    assert_eq!(
        classes(&found.exec_roots, &["codegen/other.rs", "src/lib.rs"]),
        vec![vec![PrClass::ExecSurface], Vec::new()],
        "tiering holds what the closure names"
    );
    Ok(())
}

#[test]
fn a_custom_build_script_and_a_path_attribute_are_followed() -> Result<(), Box<dyn Error>> {
    let found = reach(&[
        (
            "crates/x/Cargo.toml",
            &package("").replace("name = \"x\"\n", "name = \"x\"\nbuild = \"tools/gen.rs\"\n"),
        ),
        (
            "crates/x/tools/gen.rs",
            "#[path = \"../../shared/gen.rs\"]\nmod shared;\nmod helpers;\nfn main() {}\n",
        ),
        ("crates/shared/gen.rs", "pub fn gen() {}\n"),
        ("crates/x/tools/helpers.rs", "pub fn h() {}\n"),
    ])?;

    assert_eq!(
        found.exec_roots,
        paths(&[
            "crates/shared/gen.rs",
            "crates/x/tools/gen.rs",
            "crates/x/tools/helpers.rs",
            "crates/x/tools/helpers/mod.rs",
        ]),
        "a custom build script is a crate root; `#[path]` resolves from its directory"
    );
    Ok(())
}

#[test]
fn a_proc_macro_lib_include_outside_its_crate_is_an_exec_root() -> Result<(), Box<dyn Error>> {
    let found = reach(&[
        (
            "crates/derive/Cargo.toml",
            &package("\n[lib]\nproc-macro = true\npath = \"macros.rs\"\n")
                .replace("name = \"x\"\n", "name = \"x\"\nbuild = false\n"),
        ),
        (
            "crates/derive/macros.rs",
            "include!(\"../shared/tokens.rs\");\n",
        ),
        ("crates/shared/tokens.rs", "include!(\"more.in\");\n"),
        ("crates/shared/more.in", "fn more() {}\n"),
    ])?;

    assert_eq!(
        found.exec_roots,
        paths(&[
            "crates/derive/macros.rs",
            "crates/derive/more.in",
            "crates/shared/more.in",
            "crates/shared/tokens.rs",
        ]),
        "an included file's own include resolves from it, and from its includer to be sure"
    );
    Ok(())
}

#[test]
fn data_a_build_script_reads_is_a_root_but_not_read_as_rust() -> Result<(), Box<dyn Error>> {
    let found = reach(&[
        ("Cargo.toml", &package("")),
        (
            "build.rs",
            "const T: &str = include_str!(\"template.md\");\nfn main() {}\n",
        ),
        ("template.md", "# Modèle {{ name }}\n"),
    ])?;

    assert_eq!(
        found.exec_roots,
        paths(&["build.rs", "template.md"]),
        "its bytes steer the build script, but they are not Rust"
    );
    Ok(())
}

#[test]
fn a_symlinked_manifest_makes_everything_exec_surface() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(Revision::Head, "manifests/x.toml", &package(""));
    pr.link(
        Revision::Head,
        "crates/x/Cargo.toml",
        "../../manifests/x.toml",
    );
    pr.set(Revision::Head, "crates/x/build.rs", "fn main() {}\n");

    let found = compile_time_reach(&pr.revisions, Revision::Head)?;

    assert_eq!(
        found.exec_roots,
        [ExecRoot::Everything],
        "which build scripts exist must not depend on a link"
    );
    Ok(())
}

#[test]
fn an_include_cycle_ends() -> Result<(), Box<dyn Error>> {
    let found = reach(&[
        ("Cargo.toml", &package("")),
        ("build.rs", "include!(\"a.in\");\n"),
        ("a.in", "include!(\"build.rs\");\n"),
    ])?;

    assert_eq!(found.exec_roots, paths(&["a.in", "build.rs"]), "each once");
    Ok(())
}

#[test]
fn what_the_closure_cannot_place_makes_everything_exec_surface() -> Result<(), Box<dyn Error>> {
    for build in [
        "const S: &str = \"never ends;",
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));",
        "#[cfg_attr(unix, path = \"u.rs\")]\nmod u;",
        "#[path = \"/etc/x.rs\"]\nmod x;",
        "#[path = \"x.rs\"]\nfn f() {}",
        "fn f() {\n    mod inner;\n}",
        "macro_rules! m { () => { include!(\"x.rs\"); }; }",
        "use std::include as inc;",
        "m!($x [allow(x)]);",
        "include!(\"../../outside.rs\");",
    ] {
        let found = reach(&[("Cargo.toml", &package("")), ("build.rs", build)])?;

        assert_eq!(found.exec_roots, [ExecRoot::Everything], "{build}");
    }
    Ok(())
}

#[test]
fn a_symlinked_or_unparseable_entry_makes_everything_exec_surface() -> Result<(), Box<dyn Error>> {
    let mut linked = Pr::new()?;
    linked.set(Revision::Head, "Cargo.toml", &package(""));
    linked.link(Revision::Head, "build.rs", "tools/build.rs");
    linked.set(Revision::Head, "tools/build.rs", "fn main() {}\n");
    let unparseable = reach(&[("Cargo.toml", "[package\n")])?;

    let found = compile_time_reach(&linked.revisions, Revision::Head)?;

    assert_eq!(
        (found.exec_roots, unparseable.exec_roots),
        (vec![ExecRoot::Everything], vec![ExecRoot::Everything]),
        "a link can point anywhere; an unreadable manifest names no entry"
    );
    Ok(())
}

#[test]
fn a_module_in_a_nested_include_makes_everything_exec_surface() -> Result<(), Box<dyn Error>> {
    for build in [
        "mod gen {\n    include!(\"x.rs\");\n}\nfn main() {}\n",
        "fn main() {\n    include!(\"x.rs\");\n}\n",
    ] {
        let found = reach(&[
            ("Cargo.toml", &package("")),
            ("build.rs", build),
            ("x.rs", "mod bar;\n"),
            ("bar.rs", "pub fn bar() {}\n"),
            ("gen/bar.rs", "pub fn bar() {}\n"),
        ])?;

        assert_eq!(
            found.exec_roots,
            [ExecRoot::Everything],
            "rustc looks for `bar` in the enclosing module's directory: {build}"
        );
    }
    Ok(())
}

#[test]
fn a_symlink_or_a_submodule_is_an_unplaced_include_target() -> Result<(), Box<dyn Error>> {
    let mut linked = Pr::new()?;
    linked.set(Revision::Head, "crates/x/src/lib.rs", "pub fn f() {}\n");
    linked.link(Revision::Head, "crates/x/src/gen.rs", "../../../gen.rs");
    let mut submodule = Pr::new()?;
    submodule.set(Revision::Head, "crates/x/src/lib.rs", "pub fn f() {}\n");
    submodule.submodule(Revision::Head, "vendor/dep");

    let found = (
        compile_time_reach(&linked.revisions, Revision::Head)?.include_targets,
        compile_time_reach(&submodule.revisions, Revision::Head)?.include_targets,
    );

    assert_eq!(
        found,
        (vec![String::new()], vec![String::new()]),
        "what Rust behind a link or in a submodule includes is not seen"
    );
    Ok(())
}

#[test]
fn an_include_through_a_symlink_is_unplaced() -> Result<(), Box<dyn Error>> {
    let mut pr = Pr::new()?;
    pr.set(
        Revision::Head,
        "crates/x/src/lib.rs",
        "include!(\"gen.in\");\n",
    );
    pr.link(Revision::Head, "crates/x/src/gen.in", "../../../gen.in");
    pr.set(
        Revision::Head,
        "gen.in",
        "const D: &str = include_str!(\"d.md\");\n",
    );

    let found = compile_time_reach(&pr.revisions, Revision::Head)?;

    assert_eq!(
        found.include_targets,
        [String::new(), "crates/x/src/gen.in".to_owned()],
        "the link's target includes a file this scan does not place"
    );
    Ok(())
}

#[test]
fn code_read_under_another_name_is_scanned_too() -> Result<(), Box<dyn Error>> {
    let lib = "include!(\"gen.txt\");\n#[path = \"m.txt\"]\nmod m;\n";

    let found = reach(&[
        ("crates/x/src/lib.rs", lib),
        (
            "crates/x/src/gen.txt",
            "const D: &str = include_str!(\"d.md\");\n",
        ),
        (
            "crates/x/src/m.txt",
            "const N: &[u8] = include_bytes!(\"n.bin\");\n",
        ),
    ])?;

    assert_eq!(
        found.include_targets,
        [
            "crates/x/src/d.md",
            "crates/x/src/gen.txt",
            "crates/x/src/m.txt",
            "crates/x/src/n.bin",
        ],
        "a `.txt` file read as Rust has its own include targets"
    );
    Ok(())
}

#[test]
fn a_closure_past_its_work_budget_makes_everything_exec_surface() -> Result<(), Box<dyn Error>> {
    let build = "include!(\"x.in\");\n".repeat(40_000);

    let found = reach(&[
        ("Cargo.toml", &package("")),
        ("build.rs", &build),
        ("x.in", "fn x() {}\n"),
    ])?;

    assert_eq!(
        found.exec_roots,
        [ExecRoot::Everything],
        "one file naming too much is refused, not walked"
    );
    Ok(())
}

#[test]
fn a_path_past_the_caps_places_nothing_quickly() -> Result<(), Box<dyn Error>> {
    let deep = format!("{}deep.rs", "a/".repeat(20_000));
    let manifest = package("");
    let found = reach(&[
        ("Cargo.toml", manifest.as_str()),
        ("build.rs", "fn main() {}\n"),
        (deep.as_str(), "fn f() {}\n"),
    ])?;

    assert_eq!(
        (
            found.exec_roots,
            found.include_targets.contains(&String::new())
        ),
        (vec![ExecRoot::Everything], true),
        "a tree path past 4 KiB or 256 segments is not walked prefix by prefix"
    );
    Ok(())
}

#[test]
fn include_literals_past_the_target_budget_place_nothing() -> Result<(), Box<dyn Error>> {
    let deep = format!("{}lib.rs", "d/".repeat(250));
    let embed = |literal: String| format!("const _: &str = include_str!(\"{literal}\");\n");
    let distinct: String = (0..100_000)
        .map(|index| embed(format!("x{index}")))
        .collect();
    let repeated: String = (0..100_000).map(|_| embed("x".to_owned())).collect();

    let many = reach(&[(deep.as_str(), distinct.as_str())])?;
    let one = reach(&[(deep.as_str(), repeated.as_str())])?;

    assert_eq!(
        (
            many.include_targets.first().map(String::as_str),
            many.include_targets.len() <= 10_001,
            one.include_targets,
        ),
        (Some(""), true, vec![format!("{}x", "d/".repeat(250))]),
        "100,000 distinct literals spend the 10,000-target budget; one literal repeated is one"
    );
    Ok(())
}

#[test]
fn build_scripts_past_the_visit_cap_are_refused_before_they_resolve() -> Result<(), Box<dyn Error>>
{
    let dir = "d/".repeat(250);
    let scripts: Vec<String> = (0..100_000)
        .map(|index| format!("\"b{index}.rs\""))
        .collect();
    let manifest = format!(
        "[package]\nname = \"p\"\nbuild = [{}]\n\n[lints]\nworkspace = true\n",
        scripts.join(", ")
    );
    let path = format!("{dir}Cargo.toml");

    let found = reach(&[(path.as_str(), manifest.as_str())])?;

    assert_eq!(
        found.exec_roots,
        vec![ExecRoot::Everything],
        "more build scripts than the closure can visit place nothing"
    );
    Ok(())
}

#[test]
fn reads_past_the_byte_budget_place_nothing() -> Result<(), Box<dyn Error>> {
    let big = format!("fn x() {{}}\n//{}\n", "x".repeat(4 << 20));
    let names: Vec<String> = (0..9).map(|index| format!("g{index}.in")).collect();
    let build: String = names
        .iter()
        .map(|name| ["include!(\"", name.as_str(), "\");\n"].concat())
        .collect();
    let manifest = package("");
    let mut files = vec![
        ("Cargo.toml", manifest.as_str()),
        ("build.rs", build.as_str()),
    ];
    files.extend(names.iter().map(|name| (name.as_str(), big.as_str())));

    let found = reach(&files)?;

    assert_eq!(
        (
            found.exec_roots,
            found.include_targets.contains(&String::new())
        ),
        (vec![ExecRoot::Everything], true),
        "36 MiB of included files is past the 32 MiB the walk may read"
    );
    Ok(())
}

#[test]
fn a_cargo_target_that_is_not_rs_is_an_unplaced_include_target() -> Result<(), Box<dyn Error>> {
    for extra in [
        "\n[lib]\npath = \"lib.txt\"\n",
        "\n[[bin]]\nname = \"b\"\npath = \"main.in\"\n",
    ] {
        let found = reach(&[
            ("crates/x/Cargo.toml", &package(extra)),
            (
                "crates/x/lib.txt",
                "const D: &str = include_str!(\"d.md\");\n",
            ),
            ("crates/x/main.in", "fn main() {}\n"),
        ])?;

        assert!(
            found.include_targets.contains(&String::new()),
            "rustc compiles a file the Rust walk does not read: {extra}"
        );
    }
    Ok(())
}

#[test]
fn a_crate_without_build_time_code_has_no_exec_roots() -> Result<(), Box<dyn Error>> {
    let found = reach(&[
        ("crates/x/Cargo.toml", &package("")),
        ("crates/x/src/lib.rs", "mod a;\n"),
        ("crates/x/src/a.rs", "pub fn a() {}\n"),
        (
            "crates/y/Cargo.toml",
            &package("").replace("name = \"x\"\n", "name = \"y\"\nbuild = false\n"),
        ),
        ("crates/y/build.rs", "include!(\"src/x.rs\");\n"),
    ])?;

    assert_eq!(
        found.exec_roots,
        Vec::new(),
        "no build script, no proc-macro"
    );
    Ok(())
}
