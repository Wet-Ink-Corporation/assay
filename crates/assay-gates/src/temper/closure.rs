//! A revision's compile-time reach, for tiering (`docs/architecture/04-pipeline.md` §2.2 and
//! §2.3, the Z2s scan of 03 §2.1 step 4): the files Rust reads by name, and the code every
//! build script and proc-macro lib runs at build time, as exec roots.
//!
//! The closure starts at each package's build script (`build.rs` when present and `build` is
//! not `false`, or each path `build` names) and at each proc-macro crate's lib (`[lib] path`,
//! or `src/lib.rs`), and follows `include!`, `#[path]` and `mod`. Where rustc's reading is not
//! certain it reads every place it could be: a module's `mod` beside it and in the directory
//! named after it, and an included file's own references from it and from every file that
//! included it. What it cannot place at all gives [`ExecRoot::Everything`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use assay_domain::ExecRoot;
use toml::{Table, Value};

use super::head::{is_manifest, is_rust};
use super::reach::{FileReach, Include, scan};
use super::statics::{Files, Tree};
use super::tokens::Source;
use crate::revision::{EntryKind, Revision, RevisionFiles, TreeEntry};

/// Most files the closure visits before it gives up and returns `Everything` (H-13).
const MAX_VISITS: usize = 10_000;

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
/// symlink or a submodule, a Rust file that does not lex, and any reference the scan cannot
/// place make the closure [`ExecRoot::Everything`].
///
/// # Errors
///
/// The reader's error when the tree cannot be listed or a file cannot be read.
pub fn compile_time_reach<R: RevisionFiles>(
    revisions: &R,
    revision: Revision,
) -> Result<CompileTimeReach, R::Error> {
    let entries = revisions.list(revision, None)?;
    let wanted = |entry: &TreeEntry| {
        entry.kind == EntryKind::File && (is_rust(&entry.path) || is_manifest(&entry.path))
    };
    let selected: Vec<&TreeEntry> = entries.iter().filter(|entry| wanted(entry)).collect();
    let blobs = revisions.read(&selected)?;
    let files = Files::new(entries, blobs, wanted);
    let tree = Tree::new(files.files());
    let mut walk = Walk::new(revisions, &tree);
    let include_targets = walk.include_targets()?;
    let exec_roots = walk.closure()?;
    Ok(CompileTimeReach {
        include_targets,
        exec_roots,
    })
}

/// The include target of a reference that cannot be placed: not canonical, so it matches
/// every path.
const fn unplaced() -> String {
    String::new()
}

/// How a visited file was reached: where its `mod name;` and its literal paths resolve from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Context {
    /// The directories a `mod name;` in it looks in.
    module_dirs: BTreeSet<String>,
    /// The directories its include and `#[path]` literals resolve from.
    file_dirs: BTreeSet<String>,
}

impl Context {
    /// A crate root's: both beside it.
    fn root(path: &str) -> Self {
        let dir = directory(path);
        Self {
            module_dirs: BTreeSet::from([dir.to_owned()]),
            file_dirs: BTreeSet::from([dir.to_owned()]),
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
        }
    }

    /// An included file's: its includer's module directories, and its own directory and its
    /// includer's for literals. The includer keeps its context, so this one owns a copy.
    fn included(path: &str, includer: &Self) -> Self {
        let mut file_dirs = includer.file_dirs.clone();
        file_dirs.insert(directory(path).to_owned());
        Self {
            module_dirs: includer.module_dirs.clone(),
            file_dirs,
        }
    }

    /// Adds `other`'s directories; whether any was new.
    ///
    /// A file is walked again only when it is reached from somewhere new, so the walk ends.
    fn absorb(&mut self, other: Self) -> bool {
        let before = (self.module_dirs.len(), self.file_dirs.len());
        self.module_dirs.extend(other.module_dirs);
        self.file_dirs.extend(other.file_dirs);
        before != (self.module_dirs.len(), self.file_dirs.len())
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

/// The walk over one tree, reading files it does not hold on demand.
struct Walk<'r, 't, R> {
    revisions: &'r R,
    tree: &'t Tree<'t>,
    directories: HashSet<&'t str>,
    /// Each file's reach once read; `None` when it does not lex or is not a regular file.
    reaches: HashMap<String, Option<FileReach>>,
}

impl<'r, 't, R: RevisionFiles> Walk<'r, 't, R> {
    fn new(revisions: &'r R, tree: &'t Tree<'t>) -> Self {
        let mut directories = HashSet::from([""]);
        for entry in tree.entries() {
            let path = entry.path.as_str();
            directories.extend(
                path.match_indices('/')
                    .filter_map(|(slash, _)| path.get(..slash)),
            );
        }
        Self {
            revisions,
            tree,
            directories,
            reaches: HashMap::new(),
        }
    }

    /// The include targets of every Rust file of the tree, sorted.
    fn include_targets(&mut self) -> Result<Vec<String>, R::Error> {
        let mut targets = BTreeSet::new();
        for path in self.tree.files(is_rust) {
            self.load(path)?;
            let Some(Some(reach)) = self.reaches.get(path) else {
                targets.insert(unplaced());
                continue;
            };
            let literals = reach
                .includes
                .iter()
                .map(|(_, literal)| literal)
                .chain(reach.modules.iter().filter_map(|(_, path)| path.as_ref()));
            for literal in literals {
                let target = self.resolve(directory(path), literal);
                targets.insert(target.unwrap_or_else(unplaced));
            }
            if reach.uncertain {
                targets.insert(unplaced());
            }
        }
        Ok(targets.into_iter().collect())
    }

    /// `literal` resolved from `dir`: canonical, inside the repository, every `..` leaving a
    /// real directory; `None` otherwise.
    fn resolve(&self, dir: &str, literal: &str) -> Option<String> {
        let unsafe_text = literal.starts_with('/')
            || literal.contains('\\')
            || literal.chars().any(char::is_control);
        if unsafe_text {
            return None;
        }
        let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
        for part in literal.split('/') {
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
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    /// Whether `path` or a directory above it is a symlink or a submodule in the tree.
    fn behind_link(&self, path: &str) -> bool {
        path.match_indices('/')
            .filter_map(|(slash, _)| path.get(..slash))
            .chain([path])
            .any(|prefix| {
                self.tree
                    .entry(prefix)
                    .is_some_and(|entry| entry.kind != EntryKind::File)
            })
    }

    /// Reads and scans the regular file at `path` once, into `reaches`.
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
            (None, Some(entry)) if entry.kind == EntryKind::File => self
                .revisions
                .read(&[entry])?
                .into_iter()
                .next()
                .and_then(|bytes| of(&bytes)),
            (None, _) => None,
        };
        self.reaches.insert(path.to_owned(), reach);
        Ok(())
    }

    /// The closure of every build script and proc-macro lib, as exec roots.
    fn closure(&mut self) -> Result<Vec<ExecRoot>, R::Error> {
        let Some(entries) = self.entries() else {
            return Ok(vec![ExecRoot::Everything]);
        };
        let mut seen: BTreeMap<String, Context> = BTreeMap::new();
        let mut queue: Vec<(String, Reached)> = entries
            .into_iter()
            .map(|path| {
                let context = Context::root(&path);
                (path, Reached::Code(context))
            })
            .collect();
        let mut data: BTreeSet<String> = BTreeSet::new();
        let mut visits = 0_usize;
        while let Some((path, reached)) = queue.pop() {
            visits = visits.saturating_add(1);
            if visits > MAX_VISITS {
                return Ok(vec![ExecRoot::Everything]);
            }
            let Reached::Code(context) = reached else {
                if self.behind_link(&path) {
                    return Ok(vec![ExecRoot::Everything]);
                }
                data.insert(path);
                continue;
            };
            let merged = match seen.get_mut(&path) {
                Some(known) => {
                    if !known.absorb(context) {
                        continue;
                    }
                    known
                }
                // The map keeps its own copy of the path; the walk reads this one.
                None => seen.entry(path.clone()).or_insert(context),
            };
            match self.visit(&path, merged)? {
                Some(next) => queue.extend(next),
                None => return Ok(vec![ExecRoot::Everything]),
            }
        }
        data.extend(seen.into_keys());
        Ok(data.into_iter().map(ExecRoot::Path).collect())
    }

    /// The files `path` reaches with `context`, or `None` when it cannot be placed. A path the
    /// tree does not hold reaches nothing; it stays a root, since a PR adding it is a change
    /// there.
    fn visit(
        &mut self,
        path: &str,
        context: &Context,
    ) -> Result<Option<Vec<(String, Reached)>>, R::Error> {
        if self.behind_link(path) {
            return Ok(None);
        }
        if self.tree.entry(path).is_none() {
            return Ok(Some(Vec::new()));
        }
        self.load(path)?;
        let Some(Some(reach)) = self.reaches.get(path) else {
            return Ok(None);
        };
        if reach.uncertain {
            return Ok(None);
        }
        let mut next = Vec::new();
        for (kind, literal) in &reach.includes {
            for dir in &context.file_dirs {
                let Some(target) = self.resolve(dir, literal) else {
                    return Ok(None);
                };
                let reached = match kind {
                    Include::Code => Reached::Code(Context::included(&target, context)),
                    Include::Data => Reached::Data,
                };
                next.push((target, reached));
            }
        }
        for (name, literal) in &reach.modules {
            if let Some(literal) = literal {
                for dir in &context.file_dirs {
                    let Some(target) = self.resolve(dir, literal) else {
                        return Ok(None);
                    };
                    let reached = Reached::Code(Context::module(&target));
                    next.push((target, reached));
                }
                continue;
            }
            for dir in &context.module_dirs {
                let base = join(dir, name);
                for file in [format!("{base}.rs"), format!("{base}/mod.rs")] {
                    let reached = Reached::Code(Context::module(&file));
                    next.push((file, reached));
                }
            }
        }
        Ok(Some(next))
    }

    /// The entry files of every package's build script and proc-macro lib; `None` when one
    /// cannot be placed.
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
        for path in self.tree.files(is_manifest) {
            let manifest: Table = str::from_utf8(self.tree.bytes(path)?).ok()?.parse().ok()?;
            let dir = directory(path);
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
            Some(Value::Array(scripts)) => scripts
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
