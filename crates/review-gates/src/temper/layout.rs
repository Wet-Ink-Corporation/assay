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
    /// Every directory.
    directories: Directories<'h>,
}

impl<'h> Index<'h> {
    pub(super) fn new(entries: impl Iterator<Item = &'h TreeEntry>) -> Self {
        let mut kinds = HashMap::new();
        let mut directories = Directories::default();
        for entry in entries {
            let path = entry.path.as_str();
            kinds.insert(path, entry.kind);
            directories.add(path);
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
    ///
    /// Linear in `from` and `target`: each segment is one lookup in its parent directory.
    fn resolve(&self, from: &str, target: &str) -> Option<String> {
        if target.starts_with('/') {
            return None;
        }
        // Each resolved part, with the directory it names when it names one.
        let mut parts: Vec<(&str, Option<Dir>)> = Vec::new();
        // An entry at the root resolves from the root: no directory parts.
        if let Some((directory, _)) = from.rsplit_once('/') {
            for part in directory.split('/') {
                parts.push((part, self.directories.child(innermost(&parts), part)));
            }
        }
        for part in target.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    // Only a real directory may be left: not the root, a file or nothing.
                    let (_, Some(_)) = parts.pop()? else {
                        return None;
                    };
                }
                part => parts.push((part, self.directories.child(innermost(&parts), part))),
            }
        }
        let resolved = joined(&parts);
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

/// A directory of the tree: the root, or a node of [`Directories`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Dir {
    Root,
    Child(usize),
}

/// Every directory of the tree, each proper prefix up to a `/` of an entry's path, keyed by its
/// parent and its last segment, so a lookup hashes one segment, never a whole prefix (#184).
#[derive(Debug, Default)]
struct Directories<'h>(HashMap<(Dir, &'h str), Dir>);

impl<'h> Directories<'h> {
    /// Adds the directories `path` lies in.
    fn add(&mut self, path: &'h str) {
        let Some((directory, _)) = path.rsplit_once('/') else {
            return;
        };
        let mut parent = Dir::Root;
        for segment in directory.split('/') {
            let next = Dir::Child(self.0.len());
            parent = *self.0.entry((parent, segment)).or_insert(next);
        }
    }

    /// The directory `name` in `parent`, when both are directories.
    fn child(&self, parent: Option<Dir>, name: &str) -> Option<Dir> {
        self.0.get(&(parent?, name)).copied()
    }
}

/// The directory the resolved `parts` name, when they name one: the root for none.
fn innermost(parts: &[(&str, Option<Dir>)]) -> Option<Dir> {
    parts.last().map_or(Some(Dir::Root), |&(_, dir)| dir)
}

/// The resolved `parts` joined by `/`, built in one allocation.
fn joined(parts: &[(&str, Option<Dir>)]) -> String {
    let length = parts.iter().fold(parts.len(), |length, &(part, _)| {
        length.saturating_add(part.len())
    });
    let mut path = String::with_capacity(length);
    for (at, &(part, _)) in parts.iter().enumerate() {
        if at > 0 {
            path.push('/');
        }
        path.push_str(part);
    }
    path
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
        self.0.extend(module_files(from, name));
    }
}

/// Every file `mod name;` in the file at `from` can name, whichever way that file is read:
/// `name.rs` and `name/mod.rs`, beside it and in the directory named after it.
pub(super) fn module_files(from: &str, name: &str) -> [String; 4] {
    let (directory, file) = from.rsplit_once('/').unwrap_or(("", from));
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let beside = join(directory, name);
    let nested = join(&join(directory, stem), name);
    [
        format!("{beside}.rs"),
        format!("{beside}/mod.rs"),
        format!("{nested}.rs"),
        format!("{nested}/mod.rs"),
    ]
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::Index;
    use crate::revision::{EntryKind, TreeEntry};

    fn entry(path: String, kind: EntryKind) -> TreeEntry {
        TreeEntry {
            path,
            kind,
            object: "0".repeat(40),
            size: Some(0),
        }
    }

    /// The reading before #184: every prefix of every path in a set, and the resolved path
    /// re-joined at each `..`.
    fn reference(entries: &[TreeEntry], from: &str, target: &str) -> Option<String> {
        let kinds: HashMap<&str, EntryKind> = entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.kind))
            .collect();
        let directories: HashSet<&str> = entries
            .iter()
            .flat_map(|entry| {
                entry
                    .path
                    .match_indices('/')
                    .filter_map(|(slash, _)| entry.path.get(..slash))
            })
            .collect();
        if target.starts_with('/') {
            return None;
        }
        let mut parts: Vec<&str> = match from.rsplit_once('/') {
            Some((directory, _)) => directory.split('/').collect(),
            None => Vec::new(),
        };
        for part in target.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if !directories.contains(parts.join("/").as_str()) {
                        return None;
                    }
                    parts.pop()?;
                }
                part => parts.push(part),
            }
        }
        let resolved = parts.join("/");
        (kinds.get(resolved.as_str()) == Some(&EntryKind::File)).then_some(resolved)
    }

    #[test]
    #[cfg_attr(miri, ignore = "20,000-segment worst case; Miri is ~1000x slower")]
    fn a_link_twenty_thousand_directories_deep_resolves_quickly() {
        let depth = 20_000;
        let directory = vec!["d"; depth].join("/");
        let entries = [
            entry(format!("{directory}/link"), EntryKind::Symlink),
            entry(format!("{directory}/file"), EntryKind::File),
        ];
        let [link, file] = &entries;
        let target = format!("{}/{directory}/file", vec![".."; depth].join("/"));

        let resolved = Index::new(entries.iter()).resolve(&link.path, &target);

        assert_eq!(
            resolved.as_deref(),
            Some(file.path.as_str()),
            "the target climbs to the root and back down to the file"
        );
    }

    fn segment() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec!["a", "b", ""])
    }

    fn target_segment() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec!["a", "b", "", ".", ".."])
    }

    fn kind() -> impl Strategy<Value = EntryKind> {
        prop::sample::select(vec![
            EntryKind::File,
            EntryKind::Symlink,
            EntryKind::Submodule,
        ])
    }

    proptest! {
        #[test]
        fn resolve_matches_the_segment_join_reading(
            paths in vec((vec(segment(), 1..5), kind()), 0..12),
            from in vec(segment(), 1..5),
            target in vec(target_segment(), 0..8),
            absolute in any::<bool>(),
        ) {
            let entries: Vec<TreeEntry> = paths
                .into_iter()
                .map(|(segments, kind)| entry(segments.join("/"), kind))
                .collect();
            let from = from.join("/");
            let target = format!("{}{}", if absolute { "/" } else { "" }, target.join("/"));

            prop_assert_eq!(
                Index::new(entries.iter()).resolve(&from, &target),
                reference(&entries, &from, &target)
            );
        }
    }
}
