//! The closure's walk: from each build script and proc-macro lib through `include!`,
//! `#[path]` and `mod`, each file read with the context it was reached from.
//!
//! Bounded by a fixed work budget (H-13): every file it queues costs one unit plus one for
//! each directory its context holds, spent before the context is built, and its path's and
//! context's bytes, spent from [`MAX_QUEUED_BYTES`] as it is queued; a context holds at most
//! [`MAX_DIRS`] directories of each kind. Running out, or [`MAX_VISITS`] visits, gives
//! [`ExecRoot::Everything`].

use std::collections::{BTreeMap, BTreeSet};

use assay_domain::ExecRoot;

use super::{MAX_VISITS, Walk, directory, join};
use crate::revision::RevisionFiles;
use crate::temper::reach::{FileReach, Include};

/// Most units of work one closure spends before it gives up (H-13).
pub(super) const MAX_WORK: usize = 100_000;

/// Most directories of each kind one context holds before the closure gives up (H-13).
pub(super) const MAX_DIRS: usize = 32;

/// Most bytes of paths, each queued file's and its context's directories', one closure queues
/// before it gives up (H-13). [`MAX_WORK`] counts directories, not their length, and a path
/// can be 4 KiB long.
pub(super) const MAX_QUEUED_BYTES: usize = 32 << 20;

/// How a visited file was reached: where its `mod name;` and its literal paths resolve from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Context {
    /// The directories a `mod name;` in it looks in.
    module_dirs: BTreeSet<String>,
    /// The directories its include and `#[path]` literals resolve from.
    file_dirs: BTreeSet<String>,
    /// Whether it was included inside brackets, here or further up, where the module directory
    /// a `mod` or `#[path]` in it resolves from is not modelled.
    modules_unplaced: bool,
}

impl Context {
    /// A crate root's: both beside it.
    pub(super) fn root(path: &str) -> Self {
        let dir = directory(path);
        Self {
            module_dirs: BTreeSet::from([dir.to_owned()]),
            file_dirs: BTreeSet::from([dir.to_owned()]),
            modules_unplaced: false,
        }
    }

    /// A module file's: `mod.rs` looks beside itself; any other module, or one a `#[path]`
    /// placed, in both places it could.
    fn module(path: &str) -> Self {
        let dir = directory(path);
        let mut module_dirs = BTreeSet::from([dir.to_owned()]);
        let file = path.rsplit('/').next().unwrap_or(path);
        if file != "mod.rs" {
            let stem = file.strip_suffix(".rs").unwrap_or(file);
            module_dirs.insert(join(dir, stem));
        }
        Self {
            module_dirs,
            file_dirs: BTreeSet::from([dir.to_owned()]),
            modules_unplaced: false,
        }
    }

    /// The bytes of its directories.
    fn bytes(&self) -> usize {
        self.module_dirs
            .iter()
            .chain(&self.file_dirs)
            .fold(0, |bytes, dir| bytes.saturating_add(dir.len()))
    }

    /// The units of work queueing a file with this context costs.
    fn cost(&self) -> usize {
        self.module_dirs
            .len()
            .saturating_add(self.file_dirs.len())
            .saturating_add(1)
    }

    /// An included file's: its includer's module directories, and its own directory and its
    /// includer's for literals; its modules cannot be placed when it was included inside
    /// brackets (`nested`) or its includer's could not. The includer keeps its context, so
    /// this one owns a copy. `None` past [`MAX_DIRS`].
    pub(super) fn included(path: &str, includer: &Self, nested: bool) -> Option<Self> {
        let dir = directory(path);
        if includer.file_dirs.len() >= MAX_DIRS && !includer.file_dirs.contains(dir) {
            return None;
        }
        let mut file_dirs = includer.file_dirs.clone();
        file_dirs.insert(dir.to_owned());
        Some(Self {
            module_dirs: includer.module_dirs.clone(),
            file_dirs,
            modules_unplaced: includer.modules_unplaced || nested,
        })
    }

    /// Adds `other`'s directories and whether its modules cannot be placed; whether anything
    /// was new, or `None` past [`MAX_DIRS`].
    ///
    /// A file is walked again only when it is reached from somewhere new, so the walk ends.
    fn absorb(&mut self, other: Self) -> Option<bool> {
        let shape = |context: &Self| {
            (
                context.module_dirs.len(),
                context.file_dirs.len(),
                context.modules_unplaced,
            )
        };
        let before = shape(self);
        self.module_dirs.extend(other.module_dirs);
        self.file_dirs.extend(other.file_dirs);
        self.modules_unplaced |= other.modules_unplaced;
        let within = self.module_dirs.len() <= MAX_DIRS && self.file_dirs.len() <= MAX_DIRS;
        within.then(|| before != shape(self))
    }
}

/// How the closure reached a file.
#[derive(Debug)]
enum Reached {
    /// As Rust, from `Context`: the walk reads it.
    Code(Context),
    /// As data an `include_str!` or `include_bytes!` embeds: a root, never read as Rust.
    Data,
}

/// The files the closure has still to visit, and the work and bytes it may still spend
/// queueing more.
#[derive(Debug)]
struct Pending {
    budget: usize,
    bytes_left: usize,
    queue: Vec<(String, Reached)>,
}

impl Pending {
    /// Spends `cost`; `None` when the budget runs out.
    fn spend(&mut self, cost: usize) -> Option<()> {
        self.budget = self.budget.checked_sub(cost)?;
        Some(())
    }

    /// Queues `path`, reached as `reached`, its cost already spent, charging the bytes of the
    /// path and of its context's directories to [`MAX_QUEUED_BYTES`]; `None` past it.
    fn push(&mut self, path: String, reached: Reached) -> Option<()> {
        let context = match &reached {
            Reached::Code(context) => context.bytes(),
            Reached::Data => 0,
        };
        self.bytes_left = self
            .bytes_left
            .checked_sub(path.len().saturating_add(context))?;
        self.queue.push((path, reached));
        Some(())
    }
}

impl<R: RevisionFiles> Walk<'_, '_, R> {
    /// The closure of every build script and proc-macro lib, as exec roots.
    pub(super) fn closure(&mut self) -> Result<Vec<ExecRoot>, R::Error> {
        let everything = Ok(vec![ExecRoot::Everything]);
        let Some(entries) = self.entries() else {
            return everything;
        };
        let mut pending = Pending {
            budget: MAX_WORK,
            bytes_left: MAX_QUEUED_BYTES,
            queue: Vec::new(),
        };
        for path in entries {
            let context = Context::root(&path);
            let queued = pending
                .spend(context.cost())
                .and_then(|()| pending.push(path, Reached::Code(context)));
            if queued.is_none() {
                return everything;
            }
        }
        let mut seen: BTreeMap<String, Context> = BTreeMap::new();
        let mut data: BTreeSet<String> = BTreeSet::new();
        let mut visits = 0_usize;
        while let Some((path, reached)) = pending.queue.pop() {
            visits = visits.saturating_add(1);
            if visits > MAX_VISITS {
                return everything;
            }
            let Reached::Code(context) = reached else {
                if self.behind_link(&path) {
                    return everything;
                }
                data.insert(path);
                continue;
            };
            let merged = match seen.get_mut(&path) {
                Some(known) => match known.absorb(context) {
                    None => return everything,
                    Some(false) => continue,
                    Some(true) => known,
                },
                // The map keeps its own copy of the path; the walk reads this one.
                None => seen.entry(path.clone()).or_insert(context),
            };
            if self.visit(&path, merged, &mut pending)?.is_none() {
                return everything;
            }
        }
        data.extend(seen.into_keys());
        Ok(data.into_iter().map(ExecRoot::Path).collect())
    }

    /// Queues the files `path` reaches with `context` into `pending`, spending before each;
    /// `None` when it cannot be placed or the budget runs out. A path the tree does not hold
    /// reaches nothing; it stays a root, since a PR adding it is a change there.
    fn visit(
        &mut self,
        path: &str,
        context: &Context,
        pending: &mut Pending,
    ) -> Result<Option<()>, R::Error> {
        if self.behind_link(path) {
            return Ok(None);
        }
        if self.tree.entry(path).is_none() {
            return Ok(Some(()));
        }
        self.load(path)?;
        let Some(Some(reach)) = self.reaches.get(path) else {
            return Ok(None);
        };
        if reach.uncertain || (context.modules_unplaced && !reach.modules.is_empty()) {
            return Ok(None);
        }
        Ok(self.queue_reached(reach, context, pending))
    }

    /// Queues what `reach`, read with `context`, names into `pending`, spending before each;
    /// `None` when a literal cannot be placed or the budget runs out.
    fn queue_reached(
        &self,
        reach: &FileReach,
        context: &Context,
        pending: &mut Pending,
    ) -> Option<()> {
        for (kind, literal) in &reach.includes {
            for dir in &context.file_dirs {
                let target = self.resolve(dir, literal)?;
                let reached = match kind {
                    Include::Data => {
                        pending.spend(1)?;
                        Reached::Data
                    }
                    Include::Code | Include::NestedCode => {
                        pending.spend(context.cost().saturating_add(1))?;
                        let nested = *kind == Include::NestedCode;
                        Reached::Code(Context::included(&target, context, nested)?)
                    }
                };
                pending.push(target, reached)?;
            }
        }
        for (name, literal) in &reach.modules {
            let targets: Vec<String> = match literal {
                Some(literal) => context
                    .file_dirs
                    .iter()
                    .map(|dir| self.resolve(dir, literal))
                    .collect::<Option<_>>()?,
                None => context
                    .module_dirs
                    .iter()
                    .flat_map(|dir| {
                        let base = join(dir, name);
                        [format!("{base}.rs"), format!("{base}/mod.rs")]
                    })
                    .collect(),
            };
            for target in targets {
                let module = Context::module(&target);
                pending.spend(module.cost())?;
                pending.push(target, Reached::Code(module))?;
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_include_past_the_directory_cap_is_refused() {
        let mut wide = Context::root("a.rs");
        for index in 1..MAX_DIRS {
            wide =
                Context::included(&format!("d{index}/x.in"), &wide, false).expect("below the cap");
        }

        assert_eq!(
            (
                wide.file_dirs.len(),
                Context::included("other/x.in", &wide, false),
                Context::included("d1/y.in", &wide, false).map(|context| context.file_dirs.len())
            ),
            (MAX_DIRS, None, Some(MAX_DIRS)),
            "a new directory past the cap is refused; a known one is not"
        );
    }

    #[test]
    fn queueing_past_the_byte_budget_is_refused() {
        let context = Context::root("crates/a/build.rs");
        let path = "crates/a/build.rs".to_owned();
        let one = path.len() + context.bytes();
        let mut pending = Pending {
            budget: MAX_WORK,
            bytes_left: one + 1,
            queue: Vec::new(),
        };

        assert_eq!(
            (
                pending.push(path.clone(), Reached::Code(context.clone())),
                pending.push(path, Reached::Code(context)),
                pending.queue.len(),
            ),
            (Some(()), None, 1),
            "the path and both its context's directories are charged; the second does not fit"
        );
    }

    #[test]
    fn spending_past_the_budget_is_refused() {
        let mut pending = Pending {
            budget: 3,
            bytes_left: MAX_QUEUED_BYTES,
            queue: Vec::new(),
        };

        assert_eq!(
            (pending.spend(2), pending.spend(2), pending.budget),
            (Some(()), None, 1),
            "the second spend does not fit"
        );
    }
}
