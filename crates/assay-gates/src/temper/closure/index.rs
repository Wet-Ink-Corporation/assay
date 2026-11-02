//! The tree's directories, symlinks and submodules indexed one path segment at a time, so
//! resolving a literal or checking a path for a link costs the path's length, never its
//! length times its depth (H-13).

use std::collections::HashMap;

use super::within_caps;
use crate::revision::EntryKind;
use crate::temper::statics::Tree;

/// The repository root's directory id.
const ROOT: usize = 0;

/// One name inside a directory of the tree.
#[derive(Debug, Clone, Copy, Default)]
struct Node {
    /// Its directory id, when some entry of the tree sits below it.
    dir: Option<usize>,
    /// Whether it is an entry that is not a regular file: a symlink or a submodule.
    link: bool,
}

/// Every name of the tree, keyed by its directory's id and the name itself.
#[derive(Debug, Default)]
pub(in crate::temper) struct TreeIndex<'t> {
    names: HashMap<(usize, &'t str), Node>,
}

impl<'t> TreeIndex<'t> {
    /// The index of `tree`, built in one pass over its paths.
    pub(in crate::temper) fn new(tree: &Tree<'t>) -> Self {
        let mut index = Self::default();
        let mut dirs = ROOT;
        for entry in tree.entries() {
            let mut parent = ROOT;
            let mut segments = entry.path.split('/').peekable();
            while let Some(segment) = segments.next() {
                let node = index.names.entry((parent, segment)).or_default();
                if segments.peek().is_none() {
                    node.link |= entry.kind != EntryKind::File;
                } else {
                    parent = *node.dir.get_or_insert_with(|| {
                        dirs = dirs.saturating_add(1);
                        dirs
                    });
                }
            }
        }
        index
    }

    /// The name `segment` inside the directory `dir`, when both exist.
    fn name(&self, dir: Option<usize>, segment: &str) -> Option<Node> {
        self.names.get(&(dir?, segment)).copied()
    }

    /// `literal` resolved from `dir`: canonical, inside the repository, every `..` leaving a
    /// real directory, and within the closure's path caps as written and as resolved; `None`
    /// otherwise. A `..` or `.` in `dir` itself is taken as a name.
    pub(in crate::temper) fn resolve(&self, dir: &str, literal: &str) -> Option<String> {
        // The caps first, so a long literal is refused before it is scanned.
        let unsafe_text = !within_caps(literal)
            || literal.starts_with('/')
            || literal.contains('\\')
            || literal.chars().any(char::is_control);
        if unsafe_text {
            return None;
        }
        let mut resolved = String::with_capacity(dir.len().saturating_add(literal.len()));
        // Each segment of `resolved`: where it starts, and its directory id when it is a
        // directory of the tree.
        let mut segments: Vec<(usize, Option<usize>)> = Vec::new();
        let from_dir = dir.split('/').map(|part| (true, part));
        for (in_dir, part) in from_dir.chain(literal.split('/').map(|part| (false, part))) {
            match part {
                "" => {}
                "." if !in_dir => {}
                ".." if !in_dir => {
                    let (start, Some(_)) = segments.pop()? else {
                        return None;
                    };
                    resolved.truncate(start.saturating_sub(1));
                }
                part => {
                    let parent = segments.last().map_or(Some(ROOT), |(_, dir)| *dir);
                    let dir = self.name(parent, part).and_then(|node| node.dir);
                    if !resolved.is_empty() {
                        resolved.push('/');
                    }
                    segments.push((resolved.len(), dir));
                    resolved.push_str(part);
                }
            }
        }
        (!resolved.is_empty() && within_caps(&resolved)).then_some(resolved)
    }

    /// Whether `path` or a directory above it is a symlink or a submodule in the tree.
    pub(super) fn behind_link(&self, path: &str) -> bool {
        let mut dir = Some(ROOT);
        for segment in path.split('/') {
            let Some(node) = self.name(dir, segment) else {
                return false;
            };
            if node.link {
                return true;
            }
            dir = node.dir;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revision::TreeEntry;

    fn entry(path: &str, kind: EntryKind) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            kind,
            object: path.to_owned(),
            size: None,
        }
    }

    #[test]
    fn a_dot_dot_leaves_only_a_real_directory() {
        let entries = [entry("a/b/c.rs", EntryKind::File)];
        let tree = Tree::new(entries.iter().map(|entry| (entry, None)));
        let index = TreeIndex::new(&tree);

        assert_eq!(
            [
                index.resolve("a/b", "../x"),
                index.resolve("a", "b/../../y"),
                index.resolve("a", "n/../x"),
                index.resolve("", "../x"),
                index.resolve("a/b", "./c.rs"),
            ],
            [
                Some("a/x".to_owned()),
                Some("y".to_owned()),
                None,
                None,
                Some("a/b/c.rs".to_owned()),
            ],
            "`n` is not a directory of the tree, and nothing is above the root"
        );
    }

    #[test]
    fn a_path_through_a_link_is_behind_it() {
        let entries = [
            entry("a/link", EntryKind::Symlink),
            entry("a/file.rs", EntryKind::File),
            entry("sub", EntryKind::Submodule),
        ];
        let tree = Tree::new(entries.iter().map(|entry| (entry, None)));
        let index = TreeIndex::new(&tree);

        assert_eq!(
            [
                index.behind_link("a/link"),
                index.behind_link("a/link/x.rs"),
                index.behind_link("sub/x.rs"),
                index.behind_link("a/file.rs"),
                index.behind_link("a/new.rs"),
            ],
            [true, true, true, false, false],
            "the link itself and anything below it"
        );
    }
}
