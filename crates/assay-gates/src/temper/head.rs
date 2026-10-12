//! The head tree as the pinned `temper scan --all` cannot see it (03 §2.1 step 2): Rust in the
//! directories its walk skips, submodules, symlinks it would not resolve the way rustc does,
//! and attributes it does not read. Each is a blind spot, which fails the gate instead of
//! passing unseen.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::path::Path;

use super::attributes::{self, Expect};
use super::references::{self, Reference};
use super::tokens::{Source, Unlexable};
use crate::revision::{EntryKind, TreeEntry};

/// Directories temper's walk skips at any depth (temper 1.3.0, `walkRust`).
const SKIPPED: [&str; 4] = ["target", ".git", "node_modules", ".temper"];

/// Whether `path` names Rust source: its extension is `rs` in any case, so a `.RS` file the
/// scan's walk would pass over is still read here.
fn is_rust(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

/// The head commit's entries, each with its bytes when the checks read it: `.rs` files and
/// symlinks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeadTree(Vec<(TreeEntry, Option<Vec<u8>>)>);

/// Why a path is a blind spot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Why {
    /// A `.rs` file under a directory the scan skips.
    SkippedDirectory,
    /// A submodule: its files are not in this tree.
    Submodule,
    /// A symlink that does not resolve, inside the tree and through real directories only, to
    /// a regular file (a `.rs` one when the link's name ends in `.rs`).
    Symlink,
    /// A Rust file whose extension is not exactly `.rs` (`lib.RS`), which the scan's walk
    /// skips.
    Extension,
    /// An attribute the scan does not read.
    Attribute,
    /// An `include!` or `mod name;` whose file is not a `.rs` file of the tree the scan reads,
    /// or that names no file this check can resolve.
    Reference,
    /// A `.rs` file this adapter could not read: not listed with its bytes, or holding a
    /// literal or comment that never ends, code outside ASCII, or frontmatter.
    Unreadable,
}

/// One blind spot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Blind<'h> {
    pub(super) path: &'h str,
    pub(super) line: NonZeroU64,
    pub(super) why: Why,
    /// The source line, for an attribute.
    pub(super) quote: Option<String>,
}

/// One `expect` attribute in the head's Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpectAt<'h> {
    pub(super) file: &'h str,
    pub(super) line: NonZeroU64,
    pub(super) lints: Expect,
}

/// What reading the head's Rust found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Analysis<'h> {
    pub(super) blind: Vec<Blind<'h>>,
    pub(super) expects: Vec<ExpectAt<'h>>,
}

/// Whether the checks read `entry`'s bytes.
pub(super) fn needs_bytes(entry: &TreeEntry) -> bool {
    match entry.kind {
        EntryKind::File => is_rust(&entry.path),
        EntryKind::Symlink => true,
        EntryKind::Submodule => false,
    }
}

impl HeadTree {
    /// The tree from its `entries` and `blobs`, the bytes of the entries [`needs_bytes`]
    /// selects, in order. A blob that is missing leaves its entry unread, which
    /// [`Self::analyse`] reports.
    pub(super) fn new(entries: Vec<TreeEntry>, blobs: Vec<Vec<u8>>) -> Self {
        let mut blobs = blobs.into_iter();
        Self(
            entries
                .into_iter()
                .map(|entry| {
                    let bytes = if needs_bytes(&entry) {
                        blobs.next()
                    } else {
                        None
                    };
                    (entry, bytes)
                })
                .collect(),
        )
    }

    /// Every entry.
    pub(super) fn entries(&self) -> impl Iterator<Item = &TreeEntry> {
        self.0.iter().map(|(entry, _)| entry)
    }

    /// The blind spots and the `expect` attributes, in path order.
    pub(super) fn analyse(&self) -> Analysis<'_> {
        let mut analysis = Analysis::default();
        let index = Index::new(self.entries());
        for (entry, bytes) in &self.0 {
            let path = entry.path.as_str();
            let rust = is_rust(path);
            let why = match (entry.kind, bytes) {
                (EntryKind::Submodule, _) => Some(Why::Submodule),
                (EntryKind::File | EntryKind::Symlink, _) if rust && skipped(path) => {
                    Some(Why::SkippedDirectory)
                }
                // The scan's walk matches `.rs` exactly; `rust` matched it in any case.
                (EntryKind::File | EntryKind::Symlink, _) if rust && !exactly_rs(path) => {
                    Some(Why::Extension)
                }
                (EntryKind::Symlink, Some(target)) if index.link_resolves(path, target) => None,
                (EntryKind::Symlink, _) => Some(Why::Symlink),
                (EntryKind::File, Some(source)) if rust => {
                    read_source((path, source), &index, &mut analysis);
                    None
                }
                (EntryKind::File, None) if rust => Some(Why::Unreadable),
                (EntryKind::File, _) => None,
            };
            if let Some(why) = why {
                analysis.blind.push(Blind {
                    path,
                    line: NonZeroU64::MIN,
                    why,
                    quote: None,
                });
            }
        }
        analysis
    }
}

/// The tree's paths for resolving symlinks, each lookup in constant time.
struct Index<'h> {
    /// Every entry's path and kind.
    kinds: HashMap<&'h str, EntryKind>,
    /// Every directory: each proper prefix of an entry's path that ends at a `/`.
    directories: HashSet<&'h str>,
}

impl<'h> Index<'h> {
    fn new(entries: impl Iterator<Item = &'h TreeEntry>) -> Self {
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
    /// tree ([`Self::resolve`]), a `.rs` one when `link` is one.
    fn link_resolves(&self, link: &str, target: &[u8]) -> bool {
        str::from_utf8(target)
            .ok()
            .and_then(|target| self.resolve(link, target))
            .is_some_and(|resolved| !is_rust(link) || is_rust(&resolved))
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
        (self.kinds.get(resolved.as_str()) == Some(&EntryKind::File)).then_some(resolved)
    }

    /// Whether `path` is a regular `.rs` file of the tree that the scan reads.
    fn scanned(&self, path: &str) -> bool {
        self.kinds.get(path) == Some(&EntryKind::File) && exactly_rs(path) && !skipped(path)
    }

    /// Whether the file `reference` names from the `.rs` file at `from` is one the scan reads.
    fn pins(&self, from: &str, reference: Reference<'_>) -> bool {
        match reference {
            Reference::Include(target) => self
                .resolve(from, target)
                .is_some_and(|resolved| self.scanned(&resolved)),
            Reference::Module(name) => self.module_found(from, name),
            Reference::Unresolvable => false,
        }
    }

    /// Whether `mod name;` in the file at `from` has its file in the tree where rustc looks:
    /// beside a crate root or `mod.rs` (`name.rs` or `name/mod.rs`), otherwise in the
    /// directory named after the file. Crate roots are told by Cargo's conventions: `lib.rs`,
    /// `main.rs`, `build.rs`, and files directly in `tests`, `examples`, `benches` or `bin`.
    fn module_found(&self, from: &str, name: &str) -> bool {
        let (directory, file) = from.rsplit_once('/').unwrap_or(("", from));
        let root = matches!(file, "lib.rs" | "main.rs" | "build.rs" | "mod.rs")
            || matches!(
                directory.rsplit('/').next(),
                Some("tests" | "examples" | "benches" | "bin")
            );
        let base = match (root, file.strip_suffix(".rs")) {
            (true, _) => join(directory, name),
            (false, Some(stem)) => join(&join(directory, stem), name),
            (false, None) => return false,
        };
        self.scanned(&format!("{base}.rs")) || self.scanned(&format!("{base}/mod.rs"))
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
fn skipped(path: &str) -> bool {
    path.rsplit_once('/')
        .is_some_and(|(directory, _)| directory.split('/').any(|d| SKIPPED.contains(&d)))
}

/// Whether `path`'s extension is exactly `rs`, as the scan's walk matches it.
fn exactly_rs(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e == "rs")
}

/// Lexes the `.rs` file at `path`, recording its unread attributes, the file references the
/// tree does not back and its `expect`s, each blind line once.
fn read_source<'h>(
    (path, bytes): (&'h str, &[u8]),
    index: &Index<'_>,
    analysis: &mut Analysis<'h>,
) {
    let lossy = String::from_utf8_lossy(bytes);
    let source = Source::new(&lossy);
    let text = source.text();
    match source.tokens() {
        Ok(lexed) => {
            let mut lines: Vec<(NonZeroU64, Why)> = attributes::unread(&lexed)
                .into_iter()
                .map(|line| (line, Why::Attribute))
                .chain(
                    references::references(&lexed)
                        .into_iter()
                        .filter(|(_, reference)| !index.pins(path, *reference))
                        .map(|(line, _)| (line, Why::Reference)),
                )
                .collect();
            lines.sort_by_key(|(line, _)| *line);
            lines.dedup_by_key(|(line, _)| *line);
            analysis
                .blind
                .extend(lines.into_iter().map(|(line, why)| Blind {
                    path,
                    line,
                    why,
                    quote: line_text(text, line),
                }));
            analysis
                .expects
                .extend(
                    attributes::expects(&lexed)
                        .into_iter()
                        .map(|(line, lints)| ExpectAt {
                            file: path,
                            line,
                            lints,
                        }),
                );
        }
        Err(Unlexable(line)) => analysis.blind.push(Blind {
            path,
            line,
            why: Why::Unreadable,
            quote: None,
        }),
    }
}

/// Line `line` of `text`, when it has one.
fn line_text(text: &str, line: NonZeroU64) -> Option<String> {
    let index = usize::try_from(line.get()).ok()?.checked_sub(1)?;
    text.lines().nth(index).map(str::to_owned)
}
