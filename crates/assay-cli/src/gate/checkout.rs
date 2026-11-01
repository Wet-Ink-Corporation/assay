//! `gates-lint`'s `--head` checkout, held against the fetched head commit before anything runs
//! (#348). The job runs PR-controlled code (clippy builds the tree, the declared steps and
//! `exec` gates run in it), so a checkout that is not `ASSAY_HEAD_SHA`'s tree would be judged
//! as that commit while something else ran.
//!
//! Every entry of the head commit's tree must be in the checkout as the same kind: a regular
//! file with the same owner execute bit (the one git reads) and the same bytes, a symlink with
//! the same target, a submodule as an empty directory. The checkout may hold nothing else that
//! is not a directory, whatever `.gitignore` says, with two exceptions: its own `.git` at the
//! root, and an untracked `target/` directory at the root, where the warm `target/` is restored
//! (ADR-21) and builds write, while the tree holds nothing under `target`. Bytes are compared
//! with the blobs read from the object-only repository (no attribute, filter or `eol` applied),
//! so a checkout that smudged or converted a file is refused too (fail closed, wi-b5affa).
//! Nothing is followed: a symlink is compared as its target's text.
//!
//! The walk visits at most the tree listing's entry cap, and the blobs are read in batches
//! within its byte cap (H-13); a name that is not UTF-8 is never the tree's, so it is refused.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assay_forge::git::{BlobRef, CommitSha, EntryKind, ObjectRepo, TreeEntry, TreeLimits};

use super::PinnedError;

/// The checkout's own repository, at its root; no tree can hold it.
const GIT_DIR: &str = ".git";

/// Where builds write and the warm `target/` is restored, at the root.
const TARGET_DIR: &str = "target";

/// The owner execute bit, which git reads as a file's executable mode.
const OWNER_EXECUTE: u32 = 0o100;

/// How the checkout differs from the tree at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Difference {
    /// The tree holds it; the checkout does not.
    Missing,
    /// The checkout holds it; the tree does not.
    Untracked,
    /// Another kind of entry: a file for a symlink, a directory for a file, a submodule that
    /// is not an empty directory.
    Kind,
    /// Another owner execute bit.
    Mode,
    /// Other bytes.
    Content,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "is missing",
            Self::Untracked => "is not in the commit",
            Self::Kind => "is another kind of entry",
            Self::Mode => "has another executable bit",
            Self::Content => "has other bytes",
        })
    }
}

/// The first path at which the checkout is not the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mismatch {
    path: String,
    difference: Difference,
}

impl fmt::Display for Mismatch {
    /// `` `<path>` <difference> ``.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` {}", self.path, self.difference)
    }
}

/// What the walk found at a path it did not enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnDisk {
    File {
        executable: bool,
        size: u64,
    },
    Symlink,
    /// An empty directory where the tree holds a submodule.
    EmptyDir,
    /// A directory with entries where the tree holds a submodule.
    Dir,
    /// A FIFO, a socket or a device.
    Other,
}

/// Refuses `checkout` unless it holds exactly `head`'s tree in `repo`, read within `limits`.
///
/// # Errors
///
/// [`PinnedError::Checkout`] naming the first difference; [`PinnedError::HeadTree`] when the
/// tree cannot be listed or read, [`PinnedError::CheckoutRead`] when the checkout cannot, and
/// [`PinnedError::CheckoutTooLarge`] past the listing's entry cap.
pub(super) fn verify(
    repo: &ObjectRepo,
    head: &CommitSha,
    checkout: &Path,
    limits: TreeLimits,
) -> Result<(), PinnedError> {
    let tree = repo
        .list_tree(head, None, limits)
        .map_err(PinnedError::HeadTree)?;
    let disk = walk(checkout, &tree, limits.max_entries)?;
    // The error owns what it names.
    let refused = |mismatch| PinnedError::Checkout {
        checkout: checkout.to_path_buf(),
        head: head.clone(),
        mismatch,
    };
    let blobs = compare(&tree, &disk).map_err(refused)?;
    match same_bytes(repo, checkout, &blobs, limits)? {
        None => Ok(()),
        Some(mismatch) => Err(refused(mismatch)),
    }
}

/// Every entry of `checkout` that is not a directory it entered, by `/`-separated path: the
/// root `.git`, and an untracked root `target` directory, are skipped; a directory where
/// `tree` holds a submodule is recorded, not entered.
fn walk(
    checkout: &Path,
    tree: &[TreeEntry],
    max_entries: usize,
) -> Result<BTreeMap<String, OnDisk>, PinnedError> {
    let submodules: BTreeSet<&str> = tree
        .iter()
        .filter(|entry| entry.kind == EntryKind::Submodule)
        .map(|entry| entry.path.as_str())
        .collect();
    let tracks_target = tree
        .iter()
        .any(|entry| entry.path == TARGET_DIR || entry.path.starts_with(&format!("{TARGET_DIR}/")));
    let mut found = BTreeMap::new();
    let mut visited = 0_usize;
    let mut pending = vec![String::new()];
    while let Some(dir) = pending.pop() {
        for (name, metadata) in entries(&checkout.join(&dir))? {
            visited = visited.saturating_add(1);
            if visited > max_entries {
                return Err(PinnedError::CheckoutTooLarge { max: max_entries });
            }
            let at_root = dir.is_empty();
            if at_root && name == GIT_DIR {
                continue;
            }
            let skipped = at_root && name == TARGET_DIR && !tracks_target;
            let path = if at_root {
                name
            } else {
                format!("{dir}/{name}")
            };
            if !metadata.file_type().is_dir() {
                found.insert(path, on_disk(&metadata));
            } else if submodules.contains(path.as_str()) {
                let empty = entries(&checkout.join(&path))?.is_empty();
                found.insert(path, if empty { OnDisk::EmptyDir } else { OnDisk::Dir });
            } else if !skipped {
                pending.push(path);
            }
        }
    }
    Ok(found)
}

/// The entries of `dir`, each named (lossily: a name that is not UTF-8 matches no tree path)
/// with its own metadata, never a symlink's target's.
fn entries(dir: &Path) -> Result<Vec<(String, Metadata)>, PinnedError> {
    let refused = |source| PinnedError::CheckoutRead {
        path: dir.to_path_buf(),
        source,
    };
    fs::read_dir(dir)
        .map_err(refused)?
        .map(|entry| {
            let entry = entry.map_err(refused)?;
            let metadata = entry.metadata().map_err(refused)?;
            Ok((entry.file_name().to_string_lossy().into_owned(), metadata))
        })
        .collect()
}

/// What `metadata` (not a directory's) says the entry is.
fn on_disk(metadata: &Metadata) -> OnDisk {
    let kind = metadata.file_type();
    if kind.is_file() {
        OnDisk::File {
            executable: metadata.permissions().mode() & OWNER_EXECUTE != 0,
            size: metadata.len(),
        }
    } else if kind.is_symlink() {
        OnDisk::Symlink
    } else {
        OnDisk::Other
    }
}

/// The first difference of kind, mode, size or presence between `tree` and `disk`; otherwise
/// the entries whose bytes are still to compare (every file and symlink).
fn compare<'t>(
    tree: &'t [TreeEntry],
    disk: &BTreeMap<String, OnDisk>,
) -> Result<Vec<&'t TreeEntry>, Mismatch> {
    let mismatch = |path: &str, difference| Mismatch {
        path: path.to_owned(),
        difference,
    };
    let mut blobs = Vec::with_capacity(tree.len());
    for entry in tree {
        let difference = match (entry.kind, disk.get(&entry.path)) {
            (_, None) => Some(Difference::Missing),
            (
                EntryKind::File,
                Some(&OnDisk::File {
                    executable: false,
                    size,
                }),
            )
            | (
                EntryKind::Executable,
                Some(&OnDisk::File {
                    executable: true,
                    size,
                }),
            ) => (entry.size != Some(size)).then_some(Difference::Content),
            (EntryKind::File | EntryKind::Executable, Some(OnDisk::File { .. })) => {
                Some(Difference::Mode)
            }
            (EntryKind::Symlink, Some(OnDisk::Symlink))
            | (EntryKind::Submodule, Some(OnDisk::EmptyDir)) => None,
            (
                EntryKind::File | EntryKind::Executable | EntryKind::Symlink | EntryKind::Submodule,
                Some(OnDisk::Symlink | OnDisk::EmptyDir | OnDisk::Dir | OnDisk::Other),
            )
            | (EntryKind::Symlink | EntryKind::Submodule, Some(OnDisk::File { .. })) => {
                Some(Difference::Kind)
            }
        };
        if let Some(difference) = difference {
            return Err(mismatch(&entry.path, difference));
        }
        if entry.kind != EntryKind::Submodule {
            blobs.push(entry);
        }
    }
    let tracked: BTreeSet<&str> = tree.iter().map(|entry| entry.path.as_str()).collect();
    match disk.keys().find(|path| !tracked.contains(path.as_str())) {
        Some(path) => Err(mismatch(path, Difference::Untracked)),
        None => Ok(blobs),
    }
}

/// The first of `blobs` whose bytes in `checkout` are not its blob's, read from `repo` in
/// batches within `limits`' byte cap (one blob over it is read alone, and refused by git).
fn same_bytes(
    repo: &ObjectRepo,
    checkout: &Path,
    blobs: &[&TreeEntry],
    limits: TreeLimits,
) -> Result<Option<Mismatch>, PinnedError> {
    let mut batch: Vec<&TreeEntry> = Vec::new();
    let mut total = 0_u64;
    for entry in blobs {
        let size = entry.size.unwrap_or(0);
        if !batch.is_empty() && total.saturating_add(size) > limits.max_total_bytes {
            if let Some(mismatch) = batch_differs(repo, checkout, &batch, limits)? {
                return Ok(Some(mismatch));
            }
            batch.clear();
            total = 0;
        }
        batch.push(entry);
        total = total.saturating_add(size);
    }
    batch_differs(repo, checkout, &batch, limits)
}

/// The first of `batch` whose bytes in `checkout` are not its blob's.
fn batch_differs(
    repo: &ObjectRepo,
    checkout: &Path,
    batch: &[&TreeEntry],
    limits: TreeLimits,
) -> Result<Option<Mismatch>, PinnedError> {
    let refs: Vec<BlobRef<'_>> = batch.iter().map(|entry| BlobRef::from(*entry)).collect();
    let expected = repo
        .read_blobs(&refs, limits)
        .map_err(PinnedError::HeadTree)?;
    for (entry, expected) in batch.iter().zip(expected) {
        if checkout_bytes(checkout, entry)? != expected {
            return Ok(Some(Mismatch {
                path: entry.path.clone(),
                difference: Difference::Content,
            }));
        }
    }
    Ok(None)
}

/// The bytes of `entry` in `checkout`: a symlink's target, or at most one byte more than the
/// listed size of a file.
fn checkout_bytes(checkout: &Path, entry: &TreeEntry) -> Result<Vec<u8>, PinnedError> {
    let path = checkout.join(&entry.path);
    let read = match entry.kind {
        EntryKind::Symlink => fs::read_link(&path).map(|target| target.into_os_string().into_vec()),
        EntryKind::File | EntryKind::Executable | EntryKind::Submodule => File::open(&path)
            .and_then(|file| {
                let mut bytes = Vec::new();
                let cap = entry.size.unwrap_or(0).saturating_add(1);
                file.take(cap).read_to_end(&mut bytes).map(|_read| bytes)
            }),
    };
    read.map_err(|source| PinnedError::CheckoutRead { path, source })
}

#[cfg(test)]
mod tests;
