//! Cargo targets the temper scan cannot be shown to read (#169, 03 §2.1 step 2): a build
//! script or a target `path` that rustc would compile from a file the scan's `*.rs` walk does
//! not read, either because its name does not end in `.rs` or because the head tree does not
//! hold it.
//!
//! Refuses rather than models (Weigh-In default wi-b5affa): a path is accepted only when it
//! is a string whose last `/` segment is a stem followed by exactly `.rs` (byte for byte, so
//! `.RS` and `lib.rs/` are refused) and it resolves from the manifest's directory, inside the
//! repository and through real directories (`TreeIndex::resolve`, within its path caps), to a
//! regular file of the head tree. A value Cargo would not read as a target (a `lib` that is
//! not a table, a `path` that is not a string, a `build` that is not a bool, a string or an
//! array of strings) is refused too. A target without `path`, an absent `build` and
//! `build = false` name no file: Cargo infers a `.rs` path and refuses a missing one when it
//! resolves the invocation's targets. `build = true` names `build.rs`, which Cargo does not
//! look for until it compiles it, so it must be in the tree.
//!
//! Requiring the file in the tree is a fail-closed refusal that narrows one route (a target
//! that names a file only a build script writes); it does not stop build-script writes in
//! general. A build script can overwrite a scanned `.rs` file, or add one a later cargo run
//! discovers; build scripts are the owner-approved exec surface (05 T6, R-07), not this
//! check's.
//!
//! Every head manifest is checked whether or not the PR changed it (owner decision on #169,
//! 2026-11-01), so a target that predates the PR fails every PR until it is fixed.

use std::fmt;
use std::num::NonZeroU64;

use toml::{Table, Value};

use super::closure::TreeIndex;
use super::head::{Blind, Why};
use super::statics::Tree;

/// Which part of a manifest names a file Cargo compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The `[package]` (or `[project]`) table itself, when it is not a table.
    Package,
    /// `[package] build` (or `[project] build`).
    Build,
    /// `[lib] path`.
    Lib,
    /// A `[[bin]] path`.
    Bin,
    /// A `[[test]] path`.
    Test,
    /// A `[[bench]] path`.
    Bench,
    /// An `[[example]] path`.
    Example,
}

/// The array-of-tables kinds, each with its manifest key.
const ARRAYS: [(&str, Kind); 4] = [
    ("bin", Kind::Bin),
    ("test", Kind::Test),
    ("bench", Kind::Bench),
    ("example", Kind::Example),
];

/// One target of a manifest: its kind, and its position (from 1) among its kind's entries
/// (the scripts of a `build` array, the tables of a `[[bin]]` array).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Target {
    kind: Kind,
    entry: usize,
}

impl Target {
    /// The `at`th (from 0) entry of `kind`.
    const fn at(kind: Kind, at: usize) -> Self {
        Self {
            kind,
            entry: at.saturating_add(1),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entry = self.entry;
        match self.kind {
            Kind::Package => f.write_str("`[package]`"),
            Kind::Build => write!(f, "`build` {entry}"),
            Kind::Lib => f.write_str("`[lib]` `path`"),
            Kind::Bin => write!(f, "`[[bin]]` {entry} `path`"),
            Kind::Test => write!(f, "`[[test]]` {entry} `path`"),
            Kind::Bench => write!(f, "`[[bench]]` {entry} `path`"),
            Kind::Example => write!(f, "`[[example]]` {entry} `path`"),
        }
    }
}

/// Why a target is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Problem {
    /// Not written as Cargo reads a target.
    Shape,
    /// Its path's last segment is not a stem followed by exactly `.rs`.
    NotRust,
    /// Its path does not resolve, inside the repository and through real directories, to a
    /// regular file of the head tree.
    Absent,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shape => "not written as Cargo reads a target",
            Self::NotRust => "not a file name ending in `.rs`",
            Self::Absent => "not a regular file of this tree",
        })
    }
}

/// The head tree as seen from one manifest's directory.
struct Head<'a, 't> {
    dir: &'a str,
    tree: &'a Tree<'t>,
    index: &'a TreeIndex<'t>,
}

impl Head<'_, '_> {
    /// Why the target file `path` is refused, or `None` when the scan reads it. O(`path`'s
    /// length): `resolve` refuses a path past its caps before walking it.
    fn check(&self, path: &str) -> Option<Problem> {
        if !names_rust(path) {
            return Some(Problem::NotRust);
        }
        let resolved = self.index.resolve(self.dir, path);
        let regular = resolved.is_some_and(|resolved| self.tree.object(&resolved).is_some());
        (!regular).then_some(Problem::Absent)
    }
}

/// Whether the last `/` segment of `path` is a non-empty stem followed by exactly `.rs`, as
/// the scan's walk matches it. Unlike `Path::extension`, a trailing `/` or `/.` is not
/// stripped first.
fn names_rust(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".rs"))
        .is_some_and(|stem| !stem.is_empty())
}

/// The blind spot of the head manifest at `path`, parsed as `manifest`: its first refused
/// target, at line 1, or `None` when every target names a file the scan reads. Stops at the
/// first refused target, so a manifest costs at most the bytes of its target values.
pub(super) fn refused<'t>(
    path: &'t str,
    manifest: &Table,
    tree: &Tree<'_>,
    index: &TreeIndex<'_>,
) -> Option<Blind<'t>> {
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let head = Head { dir, tree, index };
    let (target, problem) = first_refused(manifest, &head)?;
    Some(Blind {
        path,
        line: NonZeroU64::MIN,
        why: Why::Target,
        quote: Some(format!("{target}: {problem}")),
    })
}

/// The first target of `manifest` that `head` refuses: build scripts, then `[lib]`, then each
/// array of tables in [`ARRAYS`]' order.
fn first_refused(manifest: &Table, head: &Head<'_, '_>) -> Option<(Target, Problem)> {
    for key in ["package", "project"] {
        let Some(package) = manifest.get(key) else {
            continue;
        };
        let Some(package) = package.as_table() else {
            return Some((Target::at(Kind::Package, 0), Problem::Shape));
        };
        if let Some((at, problem)) = build(package.get("build"), head) {
            return Some((Target::at(Kind::Build, at), problem));
        }
    }
    if let Some(problem) = manifest.get("lib").and_then(|lib| target_path(lib, head)) {
        return Some((Target::at(Kind::Lib, 0), problem));
    }
    for (key, kind) in ARRAYS {
        let Some(targets) = manifest.get(key) else {
            continue;
        };
        let Some(targets) = targets.as_array() else {
            return Some((Target::at(kind, 0), Problem::Shape));
        };
        let refused = targets
            .iter()
            .enumerate()
            .find_map(|(at, target)| Some((Target::at(kind, at), target_path(target, head)?)));
        if refused.is_some() {
            return refused;
        }
    }
    None
}

/// Why the target table `target` is refused: not a table, or a `path` that is not a string or
/// that `head` refuses.
fn target_path(target: &Value, head: &Head<'_, '_>) -> Option<Problem> {
    let Some(target) = target.as_table() else {
        return Some(Problem::Shape);
    };
    match target.get("path")? {
        Value::String(path) => head.check(path),
        _ => Some(Problem::Shape),
    }
}

/// The position (from 0) of the first refused script of the package's `build` value, and why.
fn build(value: Option<&Value>, head: &Head<'_, '_>) -> Option<(usize, Problem)> {
    match value? {
        Value::Boolean(false) => None,
        Value::Boolean(true) => Some((0, head.check("build.rs")?)),
        Value::String(script) => Some((0, head.check(script)?)),
        Value::Array(scripts) => scripts.iter().enumerate().find_map(|(at, script)| {
            let problem = match script {
                Value::String(script) => head.check(script)?,
                _ => Problem::Shape,
            };
            Some((at, problem))
        }),
        _ => Some((0, Problem::Shape)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revision::{EntryKind, TreeEntry};

    /// The package directory every case reads its manifest from.
    const DIR: &str = "crates/x";

    fn entry(path: &str, kind: EntryKind) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            kind,
            object: path.to_owned(),
            size: None,
        }
    }

    /// Regular files at each of `paths`.
    fn files(paths: &[&str]) -> Vec<TreeEntry> {
        paths
            .iter()
            .map(|path| entry(path, EntryKind::File))
            .collect()
    }

    /// The first target of the manifest `text` in `DIR` that is refused, in a tree of
    /// `entries`.
    fn first(entries: &[TreeEntry], text: &str) -> Option<(Target, Problem)> {
        first_in(entries, &text.parse().expect("each case is TOML"))
    }

    /// The first target of `manifest` in `DIR` that is refused, in a tree of `entries`.
    fn first_in(entries: &[TreeEntry], manifest: &Table) -> Option<(Target, Problem)> {
        let tree = Tree::new(entries.iter().map(|entry| (entry, None)));
        let index = TreeIndex::new(&tree);
        let head = Head {
            dir: DIR,
            tree: &tree,
            index: &index,
        };
        first_refused(manifest, &head)
    }

    /// `[lib] path = <value>`, whatever characters it holds.
    fn lib_path(value: &str) -> Table {
        let mut lib = Table::new();
        lib.insert("path".to_owned(), Value::from(value));
        let mut manifest = Table::new();
        manifest.insert("lib".to_owned(), Value::Table(lib));
        manifest
    }

    const fn at(kind: Kind, entry: usize) -> Target {
        Target { kind, entry }
    }

    #[test]
    fn targets_naming_rust_files_of_the_tree_pass() {
        let tree = files(&[
            "crates/x/build.rs",
            "crates/x/gen.rs",
            "crates/x/src/lib.rs",
            "crates/x/src/main.rs",
            "crates/x/tests/t.rs",
            "crates/x/benches/b.rs",
            "crates/x/examples/e.rs",
            "crates/shared/s.rs",
        ]);
        let manifest = "[package]\nname = \"x\"\nbuild = \"build.rs\"\n\n\
            [lib]\npath = \"src/lib.rs\"\n\n\
            [[bin]]\nname = \"x\"\npath = \"src/main.rs\"\n\n\
            [[bin]]\nname = \"s\"\npath = \"../shared/s.rs\"\n\n\
            [[test]]\nname = \"t\"\npath = \"./tests/t.rs\"\n\n\
            [[bench]]\nname = \"b\"\npath = \"benches/b.rs\"\n\n\
            [[example]]\nname = \"e\"\npath = \"examples//e.rs\"\n";

        assert_eq!(
            first(&tree, manifest),
            None,
            "every target is a scanned `.rs`"
        );
    }

    #[test]
    fn a_path_whose_file_name_is_not_exactly_rust_is_refused() {
        let names = [
            "src/lib.txt",
            "src/lib.RS",
            "src/lib.Rs",
            "src/lib.rs/",
            "src/lib.rs/.",
            "src/.rs",
            "src/lib.rs.txt",
            "src/lib.rs\u{200B}",
            "src/lib.r\u{0455}",
            "src/lib",
            "",
        ];
        let tree: Vec<TreeEntry> = names
            .iter()
            .map(|name| entry(&format!("{DIR}/{name}"), EntryKind::File))
            .collect();

        let seen: Vec<_> = names
            .iter()
            .map(|name| (*name, first_in(&tree, &lib_path(name))))
            .collect();

        let refused: Vec<_> = names
            .iter()
            .map(|name| (*name, Some((at(Kind::Lib, 1), Problem::NotRust))))
            .collect();
        assert_eq!(
            seen, refused,
            "only a name ending exactly `.rs` after a stem"
        );
    }

    #[test]
    fn each_kind_of_target_is_read() {
        let tree = files(&["crates/x/build.rs", "crates/x/src/main.rs"]);
        let bin = "[[bin]]\nname = \"a\"\npath = \"src/main.rs\"\n\n";
        let cases = [
            (
                "[package]\nbuild = \"build.txt\"\n".to_owned(),
                at(Kind::Build, 1),
            ),
            (
                "[project]\nbuild = \"build.txt\"\n".to_owned(),
                at(Kind::Build, 1),
            ),
            (
                "[package]\nbuild = [\"build.rs\", \"gen.txt\"]\n".to_owned(),
                at(Kind::Build, 2),
            ),
            (
                format!("{bin}[[bin]]\nname = \"b\"\npath = \"main.txt\"\n"),
                at(Kind::Bin, 2),
            ),
            (
                "[[test]]\nname = \"t\"\npath = \"t.txt\"\n".to_owned(),
                at(Kind::Test, 1),
            ),
            (
                "[[bench]]\nname = \"b\"\npath = \"b.txt\"\n".to_owned(),
                at(Kind::Bench, 1),
            ),
            (
                "[[example]]\nname = \"e\"\npath = \"e.txt\"\n".to_owned(),
                at(Kind::Example, 1),
            ),
        ];

        for (manifest, target) in cases {
            assert_eq!(
                first(&tree, &manifest),
                Some((target, Problem::NotRust)),
                "{manifest}"
            );
        }
    }

    #[test]
    fn build_true_names_a_build_rs_that_must_be_in_the_tree() {
        let without = files(&["crates/x/src/lib.rs"]);
        let with = files(&["crates/x/build.rs"]);

        assert_eq!(
            [
                first(&without, "[package]\nbuild = true\n"),
                first(&with, "[package]\nbuild = true\n"),
                first(&without, "[package]\nbuild = false\n"),
                first(&without, "[package]\nname = \"x\"\n"),
            ],
            [
                Some((at(Kind::Build, 1), Problem::Absent)),
                None,
                None,
                None
            ],
            "an earlier build script could write `build.rs`; no script, or Cargo's own look, \
             reads none"
        );
    }

    #[test]
    fn a_rust_path_that_is_not_a_regular_file_of_the_tree_is_refused() {
        let mut tree = files(&["crates/x/src/lib.rs", "crates/x/dir.rs/a.rs", "outside.rs"]);
        tree.push(entry("crates/x/src/link.rs", EntryKind::Symlink));
        tree.push(entry("crates/x/linked", EntryKind::Symlink));
        tree.push(entry("crates/x/sub", EntryKind::Submodule));
        let paths = [
            "src/gen.rs",
            "../../outside.rs/../../../x.rs",
            "../../../outside.rs",
            "/crates/x/src/lib.rs",
            "src\\lib.rs",
            "nowhere/../src/lib.rs",
            "src/link.rs",
            "linked/lib.rs",
            "linked/../src/lib.rs",
            "sub/lib.rs",
            "dir.rs",
            "src/\u{1}lib.rs",
        ];

        let seen: Vec<_> = paths
            .iter()
            .map(|path| (*path, first_in(&tree, &lib_path(path))))
            .collect();

        let refused: Vec<_> = paths
            .iter()
            .map(|path| (*path, Some((at(Kind::Lib, 1), Problem::Absent))))
            .collect();
        assert_eq!(
            seen, refused,
            "a build script can write a missing file after the scan ran"
        );
    }

    #[test]
    fn a_target_cargo_would_not_read_as_one_is_refused() {
        let tree = files(&["crates/x/src/lib.rs"]);
        let cases = [
            ("lib = \"src/lib.rs\"\n", at(Kind::Lib, 1)),
            ("[lib]\npath = 1\n", at(Kind::Lib, 1)),
            ("[bin]\npath = \"src/lib.rs\"\n", at(Kind::Bin, 1)),
            ("bin = [\"src/lib.rs\"]\n", at(Kind::Bin, 1)),
            (
                "[[test]]\nname = \"t\"\npath = [\"t.rs\"]\n",
                at(Kind::Test, 1),
            ),
            ("package = \"x\"\n", at(Kind::Package, 1)),
            ("project = 1\n", at(Kind::Package, 1)),
            ("[package]\nbuild = 1\n", at(Kind::Build, 1)),
            (
                "[package]\nbuild = { workspace = true }\n",
                at(Kind::Build, 1),
            ),
            ("[package]\nbuild = [false]\n", at(Kind::Build, 1)),
        ];

        for (manifest, target) in cases {
            assert_eq!(
                first(&tree, manifest),
                Some((target, Problem::Shape)),
                "{manifest}"
            );
        }
    }

    #[test]
    fn keys_that_name_no_target_are_not_read() {
        let tree = files(&["crates/x/src/lib.rs"]);
        let manifest = "[package]\nname = \"x\"\n\n\
            [package.metadata.x]\npath = \"notes.txt\"\nbuild = \"b.txt\"\n\n\
            [dependencies]\nd = { path = \"../d.txt\" }\n\n\
            [lib]\ncrate-type = [\"rlib\"]\n\n\
            [[bin]]\nname = \"x\"\n\n\
            [package.build-dependencies]\n";

        assert_eq!(
            first(&tree, manifest),
            None,
            "metadata, dependencies and targets without `path` name no file Cargo compiles \
             unchecked"
        );
    }

    #[test]
    fn a_manifest_is_one_blind_spot_naming_its_first_refused_target() {
        let entries = files(&["crates/x/src/lib.rs"]);
        let tree = Tree::new(entries.iter().map(|entry| (entry, None)));
        let index = TreeIndex::new(&tree);
        let manifest: Table = "[lib]\npath = \"src/lib.rs\"\n\n\
            [[bin]]\nname = \"a\"\npath = \"a.txt\"\n\n\
            [[bin]]\nname = \"b\"\npath = \"b.rs\"\n"
            .parse()
            .expect("TOML");

        let spot = refused("crates/x/Cargo.toml", &manifest, &tree, &index);

        assert_eq!(
            spot,
            Some(Blind {
                path: "crates/x/Cargo.toml",
                line: NonZeroU64::MIN,
                why: Why::Target,
                quote: Some("`[[bin]]` 1 `path`: not a file name ending in `.rs`".to_owned()),
            }),
            "the first refused target only"
        );
    }

    #[test]
    fn a_package_that_is_not_a_table_is_named_as_the_package() {
        let entries = files(&["crates/x/src/lib.rs"]);
        let tree = Tree::new(entries.iter().map(|entry| (entry, None)));
        let index = TreeIndex::new(&tree);
        let manifest: Table = "package = \"x\"\n".parse().expect("TOML");

        let quote =
            refused("crates/x/Cargo.toml", &manifest, &tree, &index).and_then(|spot| spot.quote);

        assert_eq!(
            quote.as_deref(),
            Some("`[package]`: not written as Cargo reads a target"),
            "not its `build`, which it cannot hold"
        );
    }
}
