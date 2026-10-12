//! The symlinks a manifest copy may hold as empty files (#65 review F2): only those that
//! resolve, inside the tree and without passing through another symlink, to a path that is
//! not a directory. A link to a directory would hide it from cargo's glob `members` and path
//! dependencies, and a link out of the tree names content the copy cannot hold.

use std::collections::HashSet;

use super::super::tree::{EntryKind, TreeEntry};

/// The shape of a tree that a symlink's target is resolved against.
pub(super) struct Tree<'a> {
    /// Every symlink's path.
    links: HashSet<&'a str>,
    /// Every directory's path: each proper prefix, up to a `/`, of an entry's path.
    dirs: HashSet<&'a str>,
}

impl<'a> Tree<'a> {
    /// The shape of `entries`.
    pub(super) fn new(entries: &'a [TreeEntry]) -> Self {
        let links = entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Symlink)
            .map(|entry| entry.path.as_str())
            .collect();
        let dirs = entries
            .iter()
            .flat_map(|entry| {
                entry
                    .path
                    .match_indices('/')
                    .filter_map(|(at, _)| entry.path.get(..at))
            })
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
    pub(super) fn resolve(&self, path: &str, target: &[u8]) -> Option<String> {
        let target = std::str::from_utf8(target).ok()?;
        if target.starts_with('/') {
            return None;
        }
        let mut resolved: Vec<&str> = path.split('/').collect();
        // The link's own name: its target is resolved from its directory.
        resolved.pop();
        for segment in target.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    resolved.pop()?;
                }
                name => {
                    resolved.push(name);
                    if self.links.contains(resolved.join("/").as_str()) {
                        return None;
                    }
                }
            }
        }
        let resolved = resolved.join("/");
        let leads_to_a_file = !resolved.is_empty() && !self.dirs.contains(resolved.as_str());
        leads_to_a_file.then_some(resolved)
    }
}
