//! The include targets of a whole tree: every literal path an include macro or `#[path]`
//! names, from every Rust file and from every file read as Rust under another name.
//!
//! Fails closed, giving the unplaced target `""` (which tiering matches against every path):
//! a symlink or a submodule anywhere in the tree (Rust behind it is not read), a target that
//! is a symlink or behind one, a file that does not lex or is uncertain, a literal that does
//! not resolve, a `#[path]` in a file some `include!` reads (it resolves from the including
//! module, not modelled), and more than [`MAX_VISITS`] files read.

use std::collections::{BTreeSet, HashSet};

use super::{MAX_VISITS, Walk, directory, is_rust, unplaced};
use crate::revision::{EntryKind, RevisionFiles};
use crate::temper::reach::{FileReach, Include};

/// What the walk over the tree's include targets has found so far.
#[derive(Debug, Default)]
struct Targets {
    /// The targets, sorted.
    found: BTreeSet<String>,
    /// Files still to read as Rust: tree paths and resolved targets alike, so it owns them.
    queue: Vec<String>,
    /// Files some `include!` reads.
    included: HashSet<String>,
}

impl<R: RevisionFiles> Walk<'_, '_, R> {
    /// The include targets of the tree, sorted.
    pub(super) fn include_targets(&mut self) -> Result<Vec<String>, R::Error> {
        let mut targets = Targets {
            queue: self.tree.files(is_rust).map(str::to_owned).collect(),
            ..Targets::default()
        };
        if self
            .tree
            .entries()
            .any(|entry| entry.kind != EntryKind::File)
        {
            targets.found.insert(unplaced());
        }
        let mut read: HashSet<String> = HashSet::new();
        while let Some(path) = targets.queue.pop() {
            if read.contains(&path) {
                continue;
            }
            if read.len() >= MAX_VISITS {
                targets.found.insert(unplaced());
                break;
            }
            self.load(&path)?;
            match self.reaches.get(&path) {
                Some(Some(reach)) => self.record(&path, reach, &mut targets),
                _ => {
                    targets.found.insert(unplaced());
                }
            }
            read.insert(path);
        }
        let placed_modules = targets.included.iter().all(|path| {
            self.reaches
                .get(path)
                .and_then(Option::as_ref)
                .is_none_or(|reach| reach.modules.iter().all(|(_, path)| path.is_none()))
        });
        if !placed_modules {
            targets.found.insert(unplaced());
        }
        Ok(targets.found.into_iter().collect())
    }

    /// Records the targets `reach`, the file at `path`'s, names, queueing those read as Rust.
    fn record(&self, path: &str, reach: &FileReach, targets: &mut Targets) {
        let includes = reach
            .includes
            .iter()
            .map(|(kind, literal)| (literal, *kind != Include::Data, true));
        let modules = reach
            .modules
            .iter()
            .filter_map(|(_, literal)| Some((literal.as_ref()?, true, false)));
        for (literal, is_code, is_include) in includes.chain(modules) {
            let Some(target) = self.resolve(directory(path), literal) else {
                targets.found.insert(unplaced());
                continue;
            };
            if self.behind_link(&target) {
                targets.found.insert(unplaced());
            } else if is_code && self.tree.entry(&target).is_some() {
                // One target, three owners: the result, the queue and the included set.
                if is_include {
                    targets.included.insert(target.clone());
                }
                targets.queue.push(target.clone());
            }
            targets.found.insert(target);
        }
        if reach.uncertain {
            targets.found.insert(unplaced());
        }
    }
}
