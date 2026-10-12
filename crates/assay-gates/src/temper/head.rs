//! The head tree as the pinned `temper scan --all` cannot see it (03 §2.1 step 2): Rust in the
//! directories its walk skips, submodules, symlinks, files rustc compiles that the tree does
//! not hold, attributes the scan does not read, and plain `allow`s it did not report. Each is a
//! blind spot, which fails the gate instead of passing unseen. Where rustc's reading cannot be
//! modelled exactly, the check refuses rather than models (Weigh-In default wi-b5affa).

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::path::Path;

use super::attributes::{self, Expect};
use super::layout::{Declared, Index, exactly_rs, skipped};
use super::references::{self, Reference};
use super::tokens::{Lexed, Source, Unlexable};
use crate::revision::{EntryKind, TreeEntry};

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
    /// A symlinked Rust file, or a symlink that does not resolve, inside the tree and through
    /// real directories only, to a regular file.
    Symlink,
    /// A Rust file whose extension is not exactly `.rs` (`lib.RS`), which the scan's walk
    /// skips.
    Extension,
    /// An attribute the scan does not read.
    Attribute,
    /// An `include!`, or a `mod name;` whose file is not a `.rs` file of the tree the scan
    /// reads wherever rustc can look for it, or that names no file this check can resolve.
    Reference,
    /// A plain `allow` attribute the scan did not report at its line.
    Unreported,
    /// A `.rs` file this adapter could not read: not listed with its bytes, or holding a
    /// literal or comment that never ends, code outside ASCII, a token whose boundary depends
    /// on the edition, a shebang line or frontmatter.
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
    /// The blind spots, by path and line.
    pub(super) blind: Vec<Blind<'h>>,
    pub(super) expects: Vec<ExpectAt<'h>>,
    /// Every plain `allow` attribute, as the blind spot it is unless the scan reported it.
    pub(super) allows: Vec<Blind<'h>>,
}

impl<'h> Analysis<'h> {
    /// The blind spots with the `allow`s the scan did not report (`reported`, by file and
    /// line), by path and line, one per line.
    pub(super) fn blind_with(&self, reported: &BTreeSet<(String, NonZeroU64)>) -> Vec<&Blind<'h>> {
        let reported: BTreeSet<(&str, NonZeroU64)> = reported
            .iter()
            .map(|(file, line)| (file.as_str(), *line))
            .collect();
        let mut blind: Vec<&Blind<'h>> = self
            .blind
            .iter()
            .chain(
                self.allows
                    .iter()
                    .filter(|allow| !reported.contains(&(allow.path, allow.line))),
            )
            .collect();
        blind.sort_by_key(|spot| (spot.path, spot.line));
        blind.dedup_by_key(|spot| (spot.path, spot.line));
        blind
    }
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

    /// The blind spots, the `expect` attributes and the plain `allow`s, by path and line.
    pub(super) fn analyse(&self) -> Analysis<'_> {
        let mut analysis = Analysis::default();
        let index = Index::new(self.entries());
        let mut texts: Vec<(&str, Cow<'_, str>)> = Vec::new();
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
                // rustc and the scan need not agree on which file a module is.
                (EntryKind::Symlink, _) if rust => Some(Why::Symlink),
                (EntryKind::Symlink, Some(target)) if index.link_resolves(path, target) => None,
                (EntryKind::Symlink, _) => Some(Why::Symlink),
                (EntryKind::File, Some(source)) if rust => {
                    texts.push((path, String::from_utf8_lossy(source)));
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
        read_sources(&texts, &index, &mut analysis);
        analysis.blind.sort_by_key(|spot| (spot.path, spot.line));
        analysis
    }
}

/// One `.rs` file, lexed, with its file references.
struct LexedFile<'h, 's> {
    path: &'h str,
    text: &'s str,
    lexed: Result<Vec<Lexed<'s>>, Unlexable>,
    references: Vec<(NonZeroU64, Reference<'s>)>,
}

/// Lexes every `.rs` file in `texts`, then records each one's unread attributes, the file
/// references the tree does not back, its `expect`s and its plain `allow`s. Every file is
/// lexed first, since where a `mod` looks for its file depends on whether another file's `mod`
/// can make the declaring file a module.
fn read_sources<'h>(
    texts: &[(&'h str, Cow<'_, str>)],
    index: &Index<'_>,
    analysis: &mut Analysis<'h>,
) {
    let sources: Vec<(&'h str, Source<'_>)> = texts
        .iter()
        .map(|(path, text)| (*path, Source::new(text)))
        .collect();
    let files: Vec<LexedFile<'h, '_>> = sources
        .iter()
        .map(|(path, source)| {
            let lexed = source.tokens();
            // A file that does not lex has no references; `read_source` reports it whole.
            let references = lexed
                .as_deref()
                .map_or_else(|&Unlexable(_)| Vec::new(), references::references);
            LexedFile {
                path,
                text: source.text(),
                lexed,
                references,
            }
        })
        .collect();
    let mut declared = Declared::default();
    for file in &files {
        for (_, reference) in &file.references {
            if let Reference::Module(name) = *reference {
                declared.add(file.path, name);
            }
        }
    }
    for file in &files {
        read_source(file, index, &declared, analysis);
    }
}

/// Records the findings of one lexed `.rs` file, each blind line once.
fn read_source<'h>(
    file: &LexedFile<'h, '_>,
    index: &Index<'_>,
    declared: &Declared,
    analysis: &mut Analysis<'h>,
) {
    let LexedFile {
        path,
        text,
        lexed,
        references,
    } = file;
    let path = *path;
    let lexed = match lexed {
        Ok(lexed) => lexed,
        Err(Unlexable(line)) => {
            analysis.blind.push(Blind {
                path,
                line: *line,
                why: Why::Unreadable,
                quote: None,
            });
            return;
        }
    };
    let spot = |(line, why)| Blind {
        path,
        line,
        why,
        quote: line_text(text, line),
    };
    let mut lines: Vec<(NonZeroU64, Why)> = attributes::unread(lexed)
        .into_iter()
        .map(|line| (line, Why::Attribute))
        .chain(
            references
                .iter()
                .filter(|(_, reference)| match *reference {
                    Reference::Module(name) => !index.module_found(path, name, declared),
                    Reference::Unresolvable => true,
                })
                .map(|&(line, _)| (line, Why::Reference)),
        )
        .collect();
    lines.sort_by_key(|(line, _)| *line);
    lines.dedup_by_key(|(line, _)| *line);
    analysis.blind.extend(lines.into_iter().map(spot));
    analysis.allows.extend(
        attributes::allows(lexed)
            .into_iter()
            .map(|line| spot((line, Why::Unreported))),
    );
    analysis.expects.extend(
        attributes::expects(lexed)
            .into_iter()
            .map(|(line, lints)| ExpectAt {
                file: path,
                line,
                lints,
            }),
    );
}

/// Line `line` of `text`, when it has one.
fn line_text(text: &str, line: NonZeroU64) -> Option<String> {
    let index = usize::try_from(line.get()).ok()?.checked_sub(1)?;
    text.lines().nth(index).map(str::to_owned)
}
