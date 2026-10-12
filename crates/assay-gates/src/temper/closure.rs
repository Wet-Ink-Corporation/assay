//! A revision's compile-time reach, for tiering (`docs/architecture/04-pipeline.md` §2.2 and
//! §2.3, the Z2s scan of 03 §2.1 step 4): the files Rust reads by name, and the code every
//! build script and proc-macro lib runs at build time, as exec roots.
//!
//! The closure starts at each package's build script (`build.rs` when present and `build` is
//! not `false`, or each path `build` names) and at each proc-macro crate's lib (`[lib] path`,
//! or `src/lib.rs`), and follows `include!`, `#[path]` and `mod` (`closure/visit.rs`). Where
//! rustc's reading is not certain it reads every place it could be: a module's `mod` beside it
//! and in the directory named after it, and an included file's own references from it and
//! from every file that included it. What it cannot place at all gives
//! [`ExecRoot::Everything`]. The include targets are `closure/targets.rs`'s.

use std::collections::HashMap;

use assay_domain::ExecRoot;
use toml::{Table, Value};

use super::head::{is_manifest, is_rust};
use super::reach::{FileReach, scan};
use super::statics::{BlobCount, Files, Tree};
use super::tokens::Source;
use crate::revision::{EntryKind, Revision, RevisionFiles, TreeEntry};

mod index;
mod targets;
mod visit;

use index::TreeIndex;

/// Most files the closure visits, or the include-target walk reads, before it gives up and
/// places nothing (H-13). The literals those files hold are bounded by [`MAX_TARGETS`] and,
/// in the closure, by `visit::MAX_WORK`.
const MAX_VISITS: usize = 10_000;

/// Most distinct include literals, one file's `(directory, literal)` pairs, the include-target
/// walk resolves before it gives up and places nothing (H-13).
const MAX_TARGETS: usize = 10_000;

/// Most bytes the walk reads on demand, over every file the tree does not already hold (the
/// targets of include macros and `#[path]` that are not `.rs`), before the rest are not read
/// and place nothing (H-13).
const MAX_READ_BYTES: usize = 32 << 20;

/// What one revision's Rust reads at compile time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompileTimeReach {
    /// The canonical repository paths the include macros (`include!`, `include_str!`,
    /// `include_bytes!`, `#[doc = include_str!(..)]`) and `#[path = ..]` name from any Rust
    /// file, sorted. One that cannot be placed (a computed or escaped path, one outside the
    /// repository, a call in a macro body, a renamed macro, a file that does not lex) is `""`,
    /// which is not canonical, so tiering matches it against every path.
    pub include_targets: Vec<String>,
    /// The files every build script and proc-macro lib reaches through `include!`, `#[path]`
    /// and `mod`, itself included, as [`ExecRoot::Path`]s, sorted; or only
    /// [`ExecRoot::Everything`] when the closure met something it cannot place.
    pub exec_roots: Vec<ExecRoot>,
}

/// The compile-time reach of `revision`: its include targets and the closure of its build
/// scripts and proc-macro libs. The caller unions base's and head's for a PR's `ChangeSet`.
///
/// Fails closed: an unparseable manifest, an entry or a directory on the way to one that is a
/// symlink or a submodule, a Rust file that does not lex, any reference the scan cannot
/// place, and a walk past its work budget make the closure [`ExecRoot::Everything`].
///
/// # Errors
///
/// The reader's error when the tree cannot be listed or a file cannot be read.
pub fn compile_time_reach<R: RevisionFiles>(
    revisions: &R,
    revision: Revision,
) -> Result<CompileTimeReach, R::Error> {
    let entries = revisions.list(revision, None)?;
    let nothing_placed = || CompileTimeReach {
        include_targets: vec![unplaced()],
        exec_roots: vec![ExecRoot::Everything],
    };
    if !entries.iter().all(|entry| within_caps(&entry.path)) {
        return Ok(nothing_placed());
    }
    let wanted = |entry: &TreeEntry| {
        entry.kind == EntryKind::File && (is_rust(&entry.path) || is_manifest(&entry.path))
    };
    let selected: Vec<&TreeEntry> = entries.iter().filter(|entry| wanted(entry)).collect();
    let blobs = revisions.read(&selected)?;
    let files = match Files::new(entries, blobs, wanted) {
        Ok(files) => files,
        // Which bytes belong to which file is unknown: nothing can be placed.
        Err(BlobCount { .. }) => return Ok(nothing_placed()),
    };
    let tree = Tree::new(files.files());
    let mut walk = Walk::new(revisions, &tree);
    let include_targets = walk.include_targets()?;
    let exec_roots = walk.closure()?;
    Ok(CompileTimeReach {
        include_targets,
        exec_roots,
    })
}

/// Longest path, in bytes, the walk handles: a tree entry's, an include literal's, or one
/// resolved from them (H-13).
const MAX_PATH_BYTES: usize = 4 << 10;

/// Most `/`-separated segments a path the walk handles may have (H-13).
const MAX_SEGMENTS: usize = 256;

/// Whether `path` is within [`MAX_PATH_BYTES`] and [`MAX_SEGMENTS`], so that walking its
/// prefixes is bounded. Past either cap the walk refuses the path rather than read it.
fn within_caps(path: &str) -> bool {
    path.len() <= MAX_PATH_BYTES && path.split('/').nth(MAX_SEGMENTS).is_none()
}

/// The include target of a reference that cannot be placed: not canonical, so it matches
/// every path.
const fn unplaced() -> String {
    String::new()
}

/// The walk over one tree, reading files it does not hold on demand.
struct Walk<'r, 't, R> {
    revisions: &'r R,
    tree: &'t Tree<'t>,
    index: TreeIndex<'t>,
    /// Each file's reach once read; `None` when it does not lex, is not a regular file or
    /// was past the read budget.
    reaches: HashMap<String, Option<FileReach>>,
    /// Bytes the walk may still read on demand, across every file (H-13).
    bytes_left: usize,
}

impl<'r, 't, R: RevisionFiles> Walk<'r, 't, R> {
    fn new(revisions: &'r R, tree: &'t Tree<'t>) -> Self {
        Self {
            revisions,
            tree,
            index: TreeIndex::new(tree),
            reaches: HashMap::new(),
            bytes_left: MAX_READ_BYTES,
        }
    }

    /// `literal` resolved from `dir`: canonical, inside the repository, every `..` leaving a
    /// real directory, and within [`MAX_PATH_BYTES`] and [`MAX_SEGMENTS`] as written and as
    /// resolved; `None` otherwise. Costs the length of `dir` and `literal` (`TreeIndex`).
    fn resolve(&self, dir: &str, literal: &str) -> Option<String> {
        self.index.resolve(dir, literal)
    }

    /// Whether `path` or a directory above it is a symlink or a submodule in the tree. Costs
    /// the length of `path` (`TreeIndex`).
    fn behind_link(&self, path: &str) -> bool {
        self.index.behind_link(path)
    }

    /// Reads and scans the regular file at `path` once, into `reaches`. A file the tree does
    /// not hold is read on demand and charged to [`MAX_READ_BYTES`]: by its listed size before
    /// the read, by the bytes read when no size is listed. Past the budget it is not read and
    /// reaches as a file that cannot be placed.
    fn load(&mut self, path: &str) -> Result<(), R::Error> {
        if self.reaches.contains_key(path) {
            return Ok(());
        }
        let of = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes);
            let source = Source::new(&text);
            source.tokens().ok().map(|tokens| scan(&tokens))
        };
        let reach = match (self.tree.bytes(path), self.tree.entry(path)) {
            (Some(bytes), _) => of(bytes),
            (None, Some(entry)) if entry.kind == EntryKind::File => {
                self.read(entry)?.and_then(|bytes| of(&bytes))
            }
            (None, _) => None,
        };
        self.reaches.insert(path.to_owned(), reach);
        Ok(())
    }

    /// The bytes of `entry`, spent from the read budget; `None` past it, or when the reader
    /// gives no blob.
    fn read(&mut self, entry: &TreeEntry) -> Result<Option<Vec<u8>>, R::Error> {
        if let Some(size) = entry.size {
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            if !self.spend(size) {
                return Ok(None);
            }
        }
        let Some(bytes) = self.revisions.read(&[entry])?.into_iter().next() else {
            return Ok(None);
        };
        if entry.size.is_none() && !self.spend(bytes.len()) {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    /// Spends `bytes` from the read budget; `false`, emptying it, when it holds fewer.
    fn spend(&mut self, bytes: usize) -> bool {
        if let Some(left) = self.bytes_left.checked_sub(bytes) {
            self.bytes_left = left;
            true
        } else {
            self.bytes_left = 0;
            false
        }
    }

    /// Every parsed manifest of the tree, with its directory; `None` when one does not parse.
    fn manifests(&self) -> Option<Vec<(&'t str, Table)>> {
        self.tree
            .files(is_manifest)
            .map(|path| {
                let manifest: Table = str::from_utf8(self.tree.bytes(path)?).ok()?.parse().ok()?;
                Some((directory(path), manifest))
            })
            .collect()
    }

    /// The entry files of every package's build script and proc-macro lib; `None` when one
    /// cannot be placed, or when there are more than the closure could visit ([`MAX_VISITS`]),
    /// refused before the rest resolve (H-13).
    fn entries(&self) -> Option<Vec<String>> {
        let linked = self
            .tree
            .entries()
            .any(|entry| is_manifest(&entry.path) && entry.kind != EntryKind::File);
        if linked {
            // A symlinked or submodule manifest names packages the walk cannot see.
            return None;
        }
        let mut found = Vec::new();
        for (dir, manifest) in self.manifests()? {
            if found.len() > MAX_VISITS {
                // The closure could not visit them all: refused before more resolve.
                return None;
            }
            if let Some(package) = manifest.get("package").or_else(|| manifest.get("project")) {
                found.extend(self.build_scripts(dir, package.get("build"))?);
            }
            if let Some(lib) = manifest.get("lib").filter(|lib| is_proc_macro(lib)) {
                let lib_path = match lib.get("path") {
                    None => "src/lib.rs",
                    Some(path) => path.as_str()?,
                };
                found.push(self.resolve(dir, lib_path)?);
            }
        }
        Some(found)
    }

    /// The build scripts of the package in `dir` whose `build` value is `build`.
    fn build_scripts(&self, dir: &str, build: Option<&Value>) -> Option<Vec<String>> {
        match build {
            Some(Value::Boolean(false)) => Some(Vec::new()),
            None | Some(Value::Boolean(true)) => {
                let default = join(dir, "build.rs");
                let present = self.tree.entry(&default).is_some();
                Some(present.then_some(default).into_iter().collect())
            }
            Some(Value::String(script)) => Some(vec![self.resolve(dir, script)?]),
            Some(Value::Array(scripts)) if scripts.len() <= MAX_VISITS => scripts
                .iter()
                .map(|script| self.resolve(dir, script.as_str()?))
                .collect(),
            Some(_) => None,
        }
    }
}

/// Whether `[lib]` declares a proc-macro crate, in any of Cargo's spellings.
fn is_proc_macro(lib: &Value) -> bool {
    let flag = ["proc-macro", "proc_macro"]
        .iter()
        .any(|key| lib.get(*key).and_then(Value::as_bool) == Some(true));
    let crate_type = ["crate-type", "crate_type"].iter().any(|key| {
        lib.get(*key)
            .and_then(Value::as_array)
            .is_some_and(|types| types.iter().any(|kind| kind.as_str() == Some("proc-macro")))
    });
    flag || crate_type
}

/// The directory holding `path`; `""` at the root.
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `dir/name`, or `name` at the root.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}
