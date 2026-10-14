//! The symlinks a manifest copy may hold as empty files (#65 review F2): only those that
//! resolve, inside the tree and without passing through another symlink, to a path that is
//! not a directory. A link to a directory would hide it from cargo's glob `members` and path
//! dependencies, and a link out of the tree names content the copy cannot hold.

use std::collections::{HashMap, HashSet};

use super::super::tree::{EntryKind, TreeEntry};

/// A directory of the tree: the root, or a node of [`Directories`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Dir {
    Root,
    Child(usize),
}

/// Every directory of the tree, each proper prefix up to a `/` of an entry's path, keyed by its
/// parent and its last segment, so a lookup hashes one segment, never a whole prefix (#184).
#[derive(Debug, Default)]
struct Directories<'a>(HashMap<(Dir, &'a str), Dir>);

impl<'a> Directories<'a> {
    /// Adds the directories `path` lies in.
    fn add(&mut self, path: &'a str) {
        let Some((directory, _)) = path.rsplit_once('/') else {
            return;
        };
        let mut parent = Dir::Root;
        for segment in directory.split('/') {
            let next = Dir::Child(self.0.len());
            parent = *self.0.entry((parent, segment)).or_insert(next);
        }
    }

    /// The directory `path` lies in, the root for a path without a `/`, and its name there;
    /// `None` when [`Self::add`] was not given `path`.
    fn place<'p>(&self, path: &'p str) -> Option<(Dir, &'p str)> {
        let Some((directory, name)) = path.rsplit_once('/') else {
            return Some((Dir::Root, path));
        };
        let parent = directory
            .split('/')
            .try_fold(Dir::Root, |parent, segment| {
                self.child(Some(parent), segment)
            })?;
        Some((parent, name))
    }

    /// The directory `name` in `parent`, when both are directories.
    fn child(&self, parent: Option<Dir>, name: &str) -> Option<Dir> {
        self.0.get(&(parent?, name)).copied()
    }
}

/// The shape of a tree that a symlink's target is resolved against.
pub(super) struct Tree<'a> {
    /// Every symlink, by its directory and its name.
    links: HashSet<(Dir, &'a str)>,
    /// Every directory.
    dirs: Directories<'a>,
}

impl<'a> Tree<'a> {
    /// The shape of `entries`.
    pub(super) fn new(entries: &'a [TreeEntry]) -> Self {
        let mut dirs = Directories::default();
        for entry in entries {
            dirs.add(&entry.path);
        }
        let links = entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Symlink)
            .filter_map(|entry| dirs.place(&entry.path))
            .collect();
        Self { links, dirs }
    }

    /// Where the symlink at `path`, whose blob is `target`, leads (repo-relative), when it may
    /// stand in the copy as an empty file: its target is relative UTF-8 text that, resolved
    /// segment by segment from the link's directory, never leaves the tree, never passes
    /// through or ends at a symlink, and does not end at the root or another directory.
    /// `None` refuses it. A target that names nothing in the tree stays allowed: the real
    /// build sees no file there, and the copy's empty file can only make cargo find more,
    /// never less.
    ///
    /// Linear in `path` and `target`: each segment is one lookup in its parent directory.
    pub(super) fn resolve(&self, path: &str, target: &[u8]) -> Option<String> {
        let target = std::str::from_utf8(target).ok()?;
        if target.starts_with('/') {
            return None;
        }
        // Each resolved segment, with the directory it names when it names one.
        let mut resolved: Vec<(&str, Option<Dir>)> = Vec::new();
        // The link's own name: its target is resolved from its directory.
        if let Some((directory, _)) = path.rsplit_once('/') {
            for name in directory.split('/') {
                let parent = innermost(&resolved);
                resolved.push((name, self.dirs.child(parent, name)));
            }
        }
        for segment in target.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    resolved.pop()?;
                }
                name => {
                    let parent = innermost(&resolved);
                    if parent.is_some_and(|parent| self.links.contains(&(parent, name))) {
                        return None;
                    }
                    resolved.push((name, self.dirs.child(parent, name)));
                }
            }
        }
        let leads_to_a_directory = resolved.last().is_some_and(|&(_, dir)| dir.is_some());
        let resolved = resolved
            .iter()
            .map(|&(name, _)| name)
            .collect::<Vec<_>>()
            .join("/");
        (!resolved.is_empty() && !leads_to_a_directory).then_some(resolved)
    }
}

/// The directory the `resolved` segments name, when they name one: the root for none.
fn innermost(resolved: &[(&str, Option<Dir>)]) -> Option<Dir> {
    resolved.last().map_or(Some(Dir::Root), |&(_, dir)| dir)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::Tree;
    use crate::git::tree::{EntryKind, TreeEntry};

    /// Far above what linear resolution takes at the sizes below, far below what the
    /// re-joining reading took (#184).
    const GENEROUS: Duration = Duration::from_secs(2);

    fn entry(path: String, kind: EntryKind) -> TreeEntry {
        TreeEntry {
            path,
            kind,
            object: "0".repeat(40),
            size: Some(0),
        }
    }

    /// The reading before #184: every prefix of every path in a set, and the resolved path
    /// re-joined after each segment.
    fn reference(entries: &[TreeEntry], path: &str, target: &[u8]) -> Option<String> {
        let links: HashSet<&str> = entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Symlink)
            .map(|entry| entry.path.as_str())
            .collect();
        let dirs: HashSet<&str> = entries
            .iter()
            .flat_map(|entry| {
                entry
                    .path
                    .match_indices('/')
                    .filter_map(|(at, _)| entry.path.get(..at))
            })
            .collect();
        let target = std::str::from_utf8(target).ok()?;
        if target.starts_with('/') {
            return None;
        }
        let mut resolved: Vec<&str> = path.split('/').collect();
        resolved.pop();
        for segment in target.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    resolved.pop()?;
                }
                name => {
                    resolved.push(name);
                    if links.contains(resolved.join("/").as_str()) {
                        return None;
                    }
                }
            }
        }
        let resolved = resolved.join("/");
        (!resolved.is_empty() && !dirs.contains(resolved.as_str())).then_some(resolved)
    }

    #[test]
    fn a_link_twenty_thousand_segments_deep_resolves_quickly() {
        let depth = 20_000;
        let directory = vec!["d"; depth].join("/");
        let entries = [
            entry(format!("{directory}/link"), EntryKind::Symlink),
            entry(format!("{directory}/file"), EntryKind::File),
        ];
        let [link, file] = &entries;
        let target = format!("{}/{directory}/file", vec![".."; depth].join("/"));
        let started = Instant::now();

        let resolved = Tree::new(&entries).resolve(&link.path, target.as_bytes());

        assert_eq!(resolved.as_deref(), Some(file.path.as_str()));
        assert!(
            started.elapsed() < GENEROUS,
            "resolution is linear in the path's length: {:?}",
            started.elapsed()
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
            EntryKind::Executable,
            EntryKind::Symlink,
        ])
    }

    proptest! {
        #[test]
        fn resolve_matches_the_segment_join_reading(
            paths in vec((vec(segment(), 1..5), kind()), 0..12),
            link in vec(segment(), 1..5),
            target in vec(target_segment(), 0..8),
            absolute in any::<bool>(),
        ) {
            let entries: Vec<TreeEntry> = paths
                .into_iter()
                .map(|(segments, kind)| entry(segments.join("/"), kind))
                .collect();
            let link = link.join("/");
            let target = format!("{}{}", if absolute { "/" } else { "" }, target.join("/"));

            prop_assert_eq!(
                Tree::new(&entries).resolve(&link, target.as_bytes()),
                reference(&entries, &link, target.as_bytes())
            );
        }
    }
}
