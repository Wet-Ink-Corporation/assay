//! The include targets of a whole tree: every literal path an include macro or `#[path]`
//! names, from every Rust file and from every file read as Rust under another name.
//!
//! Fails closed, giving the unplaced target `""` (which tiering matches against every path):
//! a symlink or a submodule anywhere in the tree (Rust behind it is not read), a target that
//! is a symlink or behind one, a file that does not lex or is uncertain, a literal that does
//! not resolve, a `#[path]` in a file some `include!` reads (it resolves from the including
//! module, not modelled), a manifest that does not parse or names a target or build script
//! that is not `.rs` (rustc compiles it; this walk does not read it), and more than
//! [`MAX_VISITS`] files read.

use std::collections::{BTreeSet, HashSet};

use toml::{Table, Value};

use super::{MAX_TARGETS, MAX_VISITS, Walk, directory, is_rust, unplaced};
use crate::revision::{EntryKind, RevisionFiles};
use crate::temper::layout::exactly_rs;
use crate::temper::reach::{FileReach, Include};

/// Whether every Cargo target path `manifest` names (`[lib]`, `[[bin]]`, `[[test]]`,
/// `[[bench]]` and `[[example]]` `path`, and `build`) is a string ending in `.rs`: the walk
/// reads only those, so rustc compiling any other file (#169) is refused.
fn rust_targets(manifest: &Table) -> bool {
    let is_rs = |value: &Value| value.as_str().is_some_and(exactly_rs);
    let lib = manifest.get("lib").into_iter();
    let arrays = ["bin", "test", "bench", "example"]
        .into_iter()
        .filter_map(|kind| manifest.get(kind))
        .flat_map(|targets| targets.as_array().into_iter().flatten());
    let paths_ok = lib
        .chain(arrays)
        .filter_map(|target| target.get("path"))
        .all(is_rs);
    let build = manifest
        .get("package")
        .or_else(|| manifest.get("project"))
        .and_then(|package| package.get("build"));
    let build_ok = match build {
        None | Some(Value::Boolean(_)) => true,
        Some(Value::Array(scripts)) => scripts.iter().all(is_rs),
        Some(script) => is_rs(script),
    };
    paths_ok && build_ok
}

/// What the walk over the tree's include targets has found so far.
#[derive(Debug)]
struct Targets {
    /// The targets, sorted.
    found: BTreeSet<String>,
    /// Files still to read as Rust: tree paths and resolved targets alike, so it owns them.
    queue: Vec<String>,
    /// Files some `include!` reads.
    included: HashSet<String>,
    /// Literals the walk may still resolve, out of [`MAX_TARGETS`].
    left: usize,
}

impl<R: RevisionFiles> Walk<'_, '_, R> {
    /// The include targets of the tree, sorted.
    pub(super) fn include_targets(&mut self) -> Result<Vec<String>, R::Error> {
        let mut targets = Targets {
            found: BTreeSet::new(),
            queue: self.tree.files(is_rust).map(str::to_owned).collect(),
            included: HashSet::new(),
            left: MAX_TARGETS,
        };
        let rust_targets = self
            .manifests()
            .is_some_and(|manifests| manifests.iter().all(|(_, manifest)| rust_targets(manifest)));
        if !rust_targets
            || self
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
            let Some(Some(reach)) = self.reaches.get(&path) else {
                targets.found.insert(unplaced());
                read.insert(path);
                continue;
            };
            if self.record(&path, reach, &mut targets).is_none() {
                // Out of targets: what the rest name cannot be placed.
                targets.found.insert(unplaced());
                break;
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
    /// Each distinct literal and use (code, data, module) is resolved once and spends one of
    /// [`MAX_TARGETS`]; `None` once they are spent.
    fn record(&self, path: &str, reach: &FileReach, targets: &mut Targets) -> Option<()> {
        let includes = reach
            .includes
            .iter()
            .map(|(kind, literal)| (literal.as_str(), *kind != Include::Data, true));
        let modules = reach
            .modules
            .iter()
            .filter_map(|(_, literal)| Some((literal.as_deref()?, true, false)));
        let mut distinct = HashSet::new();
        for (literal, is_code, is_include) in includes.chain(modules) {
            if !distinct.insert((literal, is_code, is_include)) {
                continue;
            }
            targets.left = targets.left.checked_sub(1)?;
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
        Some(())
    }
}
