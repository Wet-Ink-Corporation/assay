//! The head tree as the pinned `temper scan --all` cannot see it (03 §2.1 step 2): Rust in the
//! directories its walk skips, submodules, symlinks it would not resolve the way rustc does,
//! and attributes it does not read. Each is a blind spot, which fails the gate instead of
//! passing unseen.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::path::Path;

use super::attributes::{self, Expect};
use super::tokens::{self, Unterminated};
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
    /// An attribute the scan does not read.
    Attribute,
    /// A `.rs` file this adapter could not read: not listed with its bytes, or holding a
    /// literal or comment that never ends.
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
            let skipped = path
                .rsplit_once('/')
                .is_some_and(|(directory, _)| directory.split('/').any(|d| SKIPPED.contains(&d)));
            let why = match (entry.kind, bytes) {
                (EntryKind::Submodule, _) => Some(Why::Submodule),
                (EntryKind::File | EntryKind::Symlink, _) if rust && skipped => {
                    Some(Why::SkippedDirectory)
                }
                (EntryKind::Symlink, Some(target)) if index.resolves(path, target) => None,
                (EntryKind::Symlink, _) => Some(Why::Symlink),
                (EntryKind::File, Some(source)) if rust => {
                    read_source(path, source, &mut analysis);
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

    /// Whether the symlink at `link` with target `target` resolves, lexically, to a regular
    /// file of the tree: relative, never above the root, every `..` leaving a real directory,
    /// and a `.rs` file when `link` is one.
    fn resolves(&self, link: &str, target: &[u8]) -> bool {
        let Ok(target) = str::from_utf8(target) else {
            return false;
        };
        if target.starts_with('/') {
            return false;
        }
        // A link at the root resolves from the root: no directory parts.
        let mut parts: Vec<&str> = match link.rsplit_once('/') {
            Some((directory, _)) => directory.split('/').collect(),
            None => Vec::new(),
        };
        for part in target.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if !self.directories.contains(parts.join("/").as_str()) || parts.pop().is_none()
                    {
                        return false;
                    }
                }
                part => parts.push(part),
            }
        }
        let resolved = parts.join("/");
        self.kinds.get(resolved.as_str()) == Some(&EntryKind::File)
            && (!is_rust(link) || is_rust(&resolved))
    }
}

/// Lexes the `.rs` file at `path`, recording its unread attributes and its `expect`s.
fn read_source<'h>(path: &'h str, source: &[u8], analysis: &mut Analysis<'h>) {
    let text = String::from_utf8_lossy(source);
    match tokens::tokens(&text) {
        Ok(lexed) => {
            analysis
                .blind
                .extend(attributes::unread(&lexed).into_iter().map(|line| Blind {
                    path,
                    line,
                    why: Why::Attribute,
                    quote: line_text(&text, line),
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
        Err(Unterminated(line)) => analysis.blind.push(Blind {
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
