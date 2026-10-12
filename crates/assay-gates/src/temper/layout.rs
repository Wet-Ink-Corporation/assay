//! The head tree's paths, for the checks that resolve one file from another: where a symlink
//! points, and where rustc looks for the file of a `mod name;` (03 §2.1 step 2).
//!
//! Module placement follows rustc from Cargo's crate roots. A crate root is a file Cargo
//! compiles as one, directly under a directory holding a `Cargo.toml` in the tree:
//! `src/lib.rs`, `src/main.rs`, `build.rs`, `src/bin/*.rs`, `tests/*.rs`, `examples/*.rs` and
//! `benches/*.rs`, and the `main.rs` of a target directory (`src/bin/*/main.rs`,
//! `tests/*/main.rs`, `examples/*/main.rs`, `benches/*/main.rs`). A crate root or a `mod.rs`
//! looks for `name` beside itself; any other file in the directory named after it. A file
//! that is a crate root and that some `mod` could also make a module is read both ways: each
//! of its `mod name;` needs a file in both places.

use std::collections::{HashMap, HashSet};

use crate::revision::{EntryKind, TreeEntry};

/// The directories temper's walk skips at any depth (temper 1.3.0, `walkRust`).
const SKIPPED: [&str; 4] = ["target", ".git", "node_modules", ".temper"];

/// The tree's paths, each lookup in constant time.
pub(super) struct Index<'h> {
    /// Every entry's path and kind.
    kinds: HashMap<&'h str, EntryKind>,
    /// Every directory: each proper prefix of an entry's path that ends at a `/`.
    directories: HashSet<&'h str>,
}

impl<'h> Index<'h> {
    pub(super) fn new(entries: impl Iterator<Item = &'h TreeEntry>) -> Self {
        let mut kinds = HashMap::new();
        let mut directories = HashSet::new();
        for entry in entries {
            let path = entry.path.as_str();
            kinds.insert(path, entry.kind);
            directories.extend(
                path.match_indices('/')
                    .filter_map(|(slash, _)| path.get(..slash)),
            );
        }
        Self { kinds, directories }
    }

    /// Whether the symlink at `link` with target `target` resolves to a regular file of the
    /// tree ([`Self::resolve`]).
    pub(super) fn link_resolves(&self, link: &str, target: &[u8]) -> bool {
        str::from_utf8(target)
            .ok()
            .and_then(|target| self.resolve(link, target))
            .is_some()
    }

    /// The regular file of the tree that `target` names from the directory of the entry at
    /// `from`, resolved lexically: relative, never above the root, and every `..` leaving a
    /// real directory.
    fn resolve(&self, from: &str, target: &str) -> Option<String> {
        if target.starts_with('/') {
            return None;
        }
        // An entry at the root resolves from the root: no directory parts.
        let mut parts: Vec<&str> = match from.rsplit_once('/') {
            Some((directory, _)) => directory.split('/').collect(),
            None => Vec::new(),
        };
        for part in target.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if !self.directories.contains(parts.join("/").as_str()) {
                        return None;
                    }
                    parts.pop()?;
                }
                part => parts.push(part),
            }
        }
        let resolved = parts.join("/");
        self.is_file(&resolved).then_some(resolved)
    }

    fn is_file(&self, path: &str) -> bool {
        self.kinds.get(path) == Some(&EntryKind::File)
    }

    /// Whether `path` is a regular `.rs` file of the tree that the scan reads.
    pub(super) fn scanned(&self, path: &str) -> bool {
        self.is_file(path) && exactly_rs(path) && !skipped(path)
    }

    /// Whether the file at `path` is a crate root: see the module documentation.
    fn crate_root(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        crate_directories(&parts)
            .into_iter()
            .flatten()
            .any(|directory| self.is_file(&join(&directory.join("/"), "Cargo.toml")))
    }

    /// Whether `mod name;` at the top level of the file at `from` has its file in the tree
    /// wherever rustc can look for it, given the files some `mod` can make modules
    /// (`declared`).
    pub(super) fn module_found(&self, from: &str, name: &str, declared: &Declared) -> bool {
        let (directory, file) = from.rsplit_once('/').unwrap_or(("", from));
        let Some(stem) = file.strip_suffix(".rs") else {
            return false;
        };
        let beside = join(directory, name);
        let nested = join(&join(directory, stem), name);
        let found = |base: &str| {
            self.scanned(&format!("{base}.rs")) || self.scanned(&format!("{base}/mod.rs"))
        };
        let root = self.crate_root(from);
        if file == "mod.rs" || (root && !declared.0.contains(from)) {
            found(&beside)
        } else if root {
            found(&beside) && found(&nested)
        } else {
            found(&nested)
        }
    }
}

/// The directories `parts` (a path's components) would be a crate root of, one per pattern of
/// the module documentation it matches.
fn crate_directories<'p>(parts: &'p [&'p str]) -> [Option<&'p [&'p str]>; 6] {
    const TARGETS: [&str; 3] = ["tests", "examples", "benches"];
    [
        match parts {
            [crate_dir @ .., "build.rs"] => Some(crate_dir),
            _ => None,
        },
        match parts {
            [crate_dir @ .., "src", "lib.rs" | "main.rs"] => Some(crate_dir),
            _ => None,
        },
        match parts {
            [crate_dir @ .., "src", "bin", _] => Some(crate_dir),
            _ => None,
        },
        match parts {
            [crate_dir @ .., "src", "bin", _, "main.rs"] => Some(crate_dir),
            _ => None,
        },
        match parts {
            [crate_dir @ .., target, _] if TARGETS.contains(target) => Some(crate_dir),
            _ => None,
        },
        match parts {
            [crate_dir @ .., target, _, "main.rs"] if TARGETS.contains(target) => Some(crate_dir),
            _ => None,
        },
    ]
}

/// The files some `mod name;` in the tree can make a module, whichever way its own file is
/// read: `name.rs` and `name/mod.rs`, beside the declaring file and in the directory named
/// after it.
#[derive(Debug, Default)]
pub(super) struct Declared(HashSet<String>);

impl Declared {
    /// Records `mod name;` in the file at `from`.
    pub(super) fn add(&mut self, from: &str, name: &str) {
        let (directory, file) = from.rsplit_once('/').unwrap_or(("", from));
        let stem = file.strip_suffix(".rs").unwrap_or(file);
        for base in [join(directory, name), join(&join(directory, stem), name)] {
            self.0.insert(format!("{base}/mod.rs"));
            self.0.insert(format!("{base}.rs"));
        }
    }
}

/// `directory/name`, or `name` at the root.
fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

/// Whether `path` sits under a directory the scan's walk skips.
pub(super) fn skipped(path: &str) -> bool {
    path.rsplit_once('/')
        .is_some_and(|(directory, _)| directory.split('/').any(|d| SKIPPED.contains(&d)))
}

/// Whether `path`'s extension is exactly `rs`, as the scan's walk matches it.
pub(super) fn exactly_rs(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e == "rs")
}
