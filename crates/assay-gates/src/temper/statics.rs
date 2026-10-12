//! Step 4, the static checks assay runs because temper enforces them only in its hooks
//! (`docs/architecture/03-contracts.md` §2.1): the trees both checks read, and the test
//! inventory compared file by file. The manifest checks are `manifests.rs`'s.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::head::is_rust;
use super::inventory::{Inventory, inventory};
use super::layout::module_files;
use super::tokens::Source;
use crate::revision::{EntryKind, TreeEntry};

/// A tree's entries, each with its bytes when they were read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Files(Vec<(TreeEntry, Option<Vec<u8>>)>);

/// The reader returned another number of blobs than it was asked for, so which bytes belong
/// to which file is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlobCount {
    pub(super) asked: usize,
    pub(super) read: usize,
}

impl Files {
    /// The tree from its `entries` and `blobs`, the bytes of the entries `wanted` selects, in
    /// order; an error unless there is exactly one blob for each, so no file is left unread.
    pub(super) fn new(
        entries: Vec<TreeEntry>,
        blobs: Vec<Vec<u8>>,
        wanted: impl Fn(&TreeEntry) -> bool,
    ) -> Result<Self, BlobCount> {
        let asked = entries.iter().filter(|entry| wanted(entry)).count();
        if asked != blobs.len() {
            return Err(BlobCount {
                asked,
                read: blobs.len(),
            });
        }
        let mut blobs = blobs.into_iter();
        Ok(Self(
            entries
                .into_iter()
                .map(|entry| {
                    let bytes = if wanted(&entry) { blobs.next() } else { None };
                    (entry, bytes)
                })
                .collect(),
        ))
    }

    /// Every entry, with its bytes when they were read.
    pub(super) fn files(&self) -> impl Iterator<Item = (&TreeEntry, Option<&[u8]>)> {
        self.0
            .iter()
            .map(|(entry, bytes)| (entry, bytes.as_deref()))
    }
}

/// One side of the PR as step 4 reads it: every entry, and the bytes of those it read.
#[derive(Debug, Default)]
pub(super) struct Tree<'t> {
    entries: BTreeMap<&'t str, &'t TreeEntry>,
    bytes: HashMap<&'t str, &'t [u8]>,
}

impl<'t> Tree<'t> {
    pub(super) fn new(files: impl IntoIterator<Item = (&'t TreeEntry, Option<&'t [u8]>)>) -> Self {
        let mut tree = Self::default();
        for (entry, bytes) in files {
            let path = entry.path.as_str();
            tree.entries.insert(path, entry);
            if let Some(bytes) = bytes {
                tree.bytes.insert(path, bytes);
            }
        }
        tree
    }

    /// Every entry, by path.
    pub(super) fn entries(&self) -> impl Iterator<Item = &'t TreeEntry> + '_ {
        self.entries.values().copied()
    }

    /// The entry at `path`.
    pub(super) fn entry(&self, path: &str) -> Option<&'t TreeEntry> {
        self.entries.get(path).copied()
    }

    /// The object id of the regular file at `path`.
    pub(super) fn object(&self, path: &str) -> Option<&'t str> {
        self.entry(path)
            .filter(|entry| entry.kind == EntryKind::File)
            .map(|entry| entry.object.as_str())
    }

    /// The bytes of the regular file at `path`, when they were read.
    pub(super) fn bytes(&self, path: &str) -> Option<&'t [u8]> {
        self.object(path)?;
        self.bytes.get(path).copied()
    }

    /// The bytes this tree read for a file at `entry`'s path with `entry`'s object id: an
    /// unchanged file is read once, from head.
    pub(super) fn same_bytes(&self, entry: &TreeEntry) -> Option<&'t [u8]> {
        (self.object(&entry.path) == Some(entry.object.as_str()))
            .then(|| self.bytes(&entry.path))
            .flatten()
    }

    /// The paths of the regular files `wanted` selects.
    pub(super) fn files(&self, wanted: fn(&str) -> bool) -> impl Iterator<Item = &'t str> + '_ {
        self.entries
            .iter()
            .filter(move |(path, entry)| entry.kind == EntryKind::File && wanted(path))
            .map(|(path, _)| *path)
    }
}

/// Whether the base entry `entry` must be read for step 4, given the head tree: a manifest,
/// or a Rust file that head does not hold unchanged.
pub(super) fn base_needs_bytes(entry: &TreeEntry, head: &Tree<'_>) -> bool {
    entry.kind == EntryKind::File
        && (super::head::is_manifest(&entry.path)
            || (is_rust(&entry.path) && head.object(&entry.path) != Some(entry.object.as_str())))
}

/// What one file lost from its test inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Loss {
    /// Fewer unconditional tests.
    Tests { from: u64, to: u64 },
    /// A test under these conditions is gone, or its conditions changed.
    Conditional(String),
    /// More ignores.
    Ignores { from: u64, to: u64 },
    /// A new condition on a module or the file (its text, as many times as it was added).
    Gate(String),
    /// A `mod name;` no longer declared while a file it named still holds tests.
    Module(String),
    /// The base file did not lex, so its tests could not be counted.
    Unreadable,
}

/// One file whose test inventory lost something, with the blob ids that bind an allowlist
/// entry to this exact change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InventoryChange<'t> {
    pub(super) path: &'t str,
    pub(super) base: Option<&'t str>,
    pub(super) head: Option<&'t str>,
    pub(super) losses: Vec<Loss>,
}

/// The inventory of the Rust file at `path` in `tree`: empty when there is no regular file
/// there; `None` when its bytes were not read or it does not lex.
fn counted(tree: &Tree<'_>, path: &str) -> Option<Inventory> {
    if tree.object(path).is_none() {
        return Some(Inventory::default());
    }
    let text = String::from_utf8_lossy(tree.bytes(path)?);
    let source = Source::new(&text);
    source.tokens().ok().map(|tokens| inventory(&tokens))
}

/// Every Rust file whose test inventory lost something between `base` and `head`, by path.
pub(super) fn test_inventory<'t>(base: &Tree<'t>, head: &Tree<'t>) -> Vec<InventoryChange<'t>> {
    let paths: BTreeSet<&'t str> = base.files(is_rust).chain(head.files(is_rust)).collect();
    paths
        .into_iter()
        .filter(|path| base.object(path) != head.object(path))
        .filter_map(|path| {
            let losses = losses(base, head, path);
            (!losses.is_empty()).then(|| InventoryChange {
                path,
                base: base.object(path),
                head: head.object(path),
                losses,
            })
        })
        .collect()
}

/// What the file at `path` lost between `base` and `head`. A head file that does not lex or
/// was not read is already a `blind-spot`, which no waiver clears, so it adds no loss here.
fn losses(base: &Tree<'_>, head: &Tree<'_>, path: &str) -> Vec<Loss> {
    let (before, after) = match (counted(base, path), counted(head, path)) {
        (Some(before), Some(after)) => (before, after),
        (_, None) => return Vec::new(),
        (None, Some(_)) => return vec![Loss::Unreadable],
    };
    let mut losses = Vec::new();
    if after.tests < before.tests {
        losses.push(Loss::Tests {
            from: before.tests,
            to: after.tests,
        });
    }
    if after.ignores > before.ignores {
        losses.push(Loss::Ignores {
            from: before.ignores,
            to: after.ignores,
        });
    }
    losses.extend(added(&after.conditional, &before.conditional).map(Loss::Conditional));
    losses.extend(added(&before.gates, &after.gates).map(Loss::Gate));
    losses.extend(
        added(&after.modules, &before.modules)
            .filter(|name| still_tested(base, head, path, name))
            .map(Loss::Module),
    );
    losses
}

/// Each text of `after` beyond as many times as `before` holds it.
fn added(before: &[String], after: &[String]) -> impl Iterator<Item = String> {
    let mut left: HashMap<&str, usize> = HashMap::new();
    for text in before {
        let count = left.entry(text).or_default();
        *count = count.saturating_add(1);
    }
    let mut extra = Vec::new();
    for text in after {
        match left.get_mut(text.as_str()) {
            Some(count) if *count > 0 => *count = count.saturating_sub(1),
            // The text outlives the counts; the loss owns its copy.
            _ => extra.push(text.clone()),
        }
    }
    extra.into_iter()
}

/// Most files [`still_tested`] reads below one dropped module before it holds the module tested
/// (H-13).
const MAX_MODULE_FILES: usize = 1_000;

/// Whether a file the dropped `mod name;` of the file at `from` compiled at base, directly or
/// through the modules below it, is still in the head tree and held tests at base (or could
/// not be counted there). Every place rustc could look is followed, and a walk past
/// [`MAX_MODULE_FILES`] counts as tested.
fn still_tested(base: &Tree<'_>, head: &Tree<'_>, from: &str, name: &str) -> bool {
    let mut queue: Vec<String> = module_files(from, name).into();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(file) = queue.pop() {
        if seen.contains(&file) || base.entry(&file).is_none() {
            continue;
        }
        if seen.len() >= MAX_MODULE_FILES {
            return true;
        }
        let Some(found) = counted(base, &file) else {
            return true;
        };
        let tested = found.tests > 0 || !found.conditional.is_empty();
        if tested && head.entry(&file).is_some() {
            return true;
        }
        queue.extend(
            found
                .modules
                .iter()
                .flat_map(|module| module_files(&file, module)),
        );
        seen.insert(file);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, object: &str) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            kind: EntryKind::File,
            object: object.to_owned(),
            size: None,
        }
    }

    #[test]
    fn a_text_added_twice_is_added_once_more_than_it_was() {
        let before = vec!["a".to_owned()];
        let after = vec!["a".to_owned(), "a".to_owned(), "b".to_owned()];

        assert_eq!(
            added(&before, &after).collect::<Vec<_>>(),
            ["a", "b"],
            "one `a` was there already"
        );
    }

    #[test]
    fn another_number_of_blobs_than_wanted_files_is_an_error() {
        let entries = vec![entry("a.rs", "1"), entry("b.rs", "2")];

        assert_eq!(
            Files::new(entries, vec![b"x".to_vec()], |_| true),
            Err(BlobCount { asked: 2, read: 1 }),
            "`b.rs` would read as empty"
        );
    }

    #[test]
    fn a_base_file_whose_bytes_were_not_read_is_unreadable() {
        let before = entry("t.rs", "1");
        let after = entry("t.rs", "2");
        let base = Tree::new([(&before, None)]);
        let head = Tree::new([(&after, Some(&b"fn f() {}"[..]))]);

        assert_eq!(
            test_inventory(&base, &head)
                .into_iter()
                .map(|change| change.losses)
                .collect::<Vec<_>>(),
            [vec![Loss::Unreadable]],
            "not read is not empty"
        );
    }

    #[test]
    fn an_unchanged_file_is_not_compared() {
        let file = entry("t.rs", "1");
        let tree = Tree::new([(&file, Some(&b"#[test] fn f() {}"[..]))]);
        let empty = Tree::default();

        assert_eq!(
            (
                test_inventory(&tree, &tree),
                test_inventory(&tree, &empty).len()
            ),
            (Vec::new(), 1),
            "same blob, same inventory; removed, one change"
        );
    }
}
