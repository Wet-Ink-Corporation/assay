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
//! (ADR-21) and builds write, while the tree holds nothing under `target`. That directory is
//! not examined: what the workflow restored there is believed (a residual, 03 §2.1). Bytes are
//! compared with the blobs read from the object-only repository (no attribute, filter or `eol`
//! applied), so a checkout that smudged or converted a file is refused too (fail closed,
//! wi-b5affa). Nothing is followed: a symlink is compared as its target's text. A name that is
//! not UTF-8 is never the tree's, so it is refused as untracked (named by its lossy text).
//!
//! The walk is bounded (H-13) as it reads, one entry at a time and before any entry's
//! metadata: entries that are not directories (files, symlinks, submodule directories) count
//! against the tree listing's entry cap, which counts the same kinds; directories count against
//! their own cap, every directory the tree implies plus [`UNTRACKED_DIRECTORIES`] more. The
//! blobs are read in batches within the listing's byte cap.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, DirEntry, File, Metadata};
use std::io::{self, Read};
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

/// Directories the walk enters beyond those the tree implies: the root `target`, and empty
/// directories a checkout of another commit left behind (git removes no directory it did not
/// empty itself). Past them the checkout is refused as too large.
pub(super) const UNTRACKED_DIRECTORIES: usize = 1024;

/// How much of the checkout the walk reads (H-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Caps {
    /// Entries that are not directories, or are a submodule's: the listing's entry cap.
    pub(super) files: usize,
    /// Directories entered or recorded, the root not counted.
    pub(super) directories: usize,
}

impl Caps {
    /// The caps for `tree`, listed within `max_entries`: that many files, and every directory
    /// the tree implies plus [`UNTRACKED_DIRECTORIES`].
    pub(super) fn for_tree(tree: &[TreeEntry], max_entries: usize) -> Self {
        let implied: BTreeSet<&str> = tree
            .iter()
            .flat_map(|entry| {
                entry
                    .path
                    .match_indices('/')
                    .filter_map(|(at, _)| entry.path.get(..at))
            })
            .collect();
        Self {
            files: max_entries,
            directories: implied.len().saturating_add(UNTRACKED_DIRECTORIES),
        }
    }
}

/// Why the walk stopped.
#[derive(Debug)]
pub(super) enum Stop {
    /// The checkout could not be read, or is past a cap.
    Refused(PinnedError),
    /// The checkout holds what the tree cannot.
    Differs(Mismatch),
}

impl From<PinnedError> for Stop {
    fn from(error: PinnedError) -> Self {
        Self::Refused(error)
    }
}

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
/// [`PinnedError::CheckoutTooLarge`] past the [`Caps`].
pub(super) fn verify(
    repo: &ObjectRepo,
    head: &CommitSha,
    checkout: &Path,
    limits: TreeLimits,
) -> Result<(), PinnedError> {
    let tree = repo
        .list_tree(head, None, limits)
        .map_err(PinnedError::HeadTree)?;
    // The error owns what it names.
    let refused = |mismatch| PinnedError::Checkout {
        checkout: checkout.to_path_buf(),
        head: head.clone(),
        mismatch,
    };
    let disk = match walk(checkout, &tree, Caps::for_tree(&tree, limits.max_entries)) {
        Ok(disk) => disk,
        Err(Stop::Refused(error)) => return Err(error),
        Err(Stop::Differs(mismatch)) => return Err(refused(mismatch)),
    };
    let blobs = compare(&tree, &disk).map_err(refused)?;
    match same_bytes(repo, checkout, &blobs, limits)? {
        None => Ok(()),
        Some(mismatch) => Err(refused(mismatch)),
    }
}

/// Every entry of `checkout` that is not a directory it entered, by `/`-separated path: the
/// root `.git`, and an untracked root `target` directory, are skipped; a directory where
/// `tree` holds a submodule is recorded, not entered. Each directory is read lazily and each
/// entry counted against `caps` as it is read, before its metadata.
fn walk(checkout: &Path, tree: &[TreeEntry], caps: Caps) -> Result<BTreeMap<String, OnDisk>, Stop> {
    let mut walk = Walk {
        checkout,
        submodules: tree
            .iter()
            .filter(|entry| entry.kind == EntryKind::Submodule)
            .map(|entry| entry.path.as_str())
            .collect(),
        tracks_target: tree.iter().any(|entry| {
            entry.path == TARGET_DIR || entry.path.starts_with(&format!("{TARGET_DIR}/"))
        }),
        caps,
        files: 0,
        directories: 0,
        found: BTreeMap::new(),
    };
    let mut pending = vec![String::new()];
    while let Some(dir) = pending.pop() {
        let at = checkout.join(&dir);
        for entry in fs::read_dir(&at).map_err(read_error(&at))? {
            let entry = entry.map_err(read_error(&at))?;
            pending.extend(walk.visit(&dir, &entry)?);
        }
    }
    Ok(walk.found)
}

/// The walk's state: what it has counted and found.
struct Walk<'t> {
    checkout: &'t Path,
    submodules: BTreeSet<&'t str>,
    tracks_target: bool,
    caps: Caps,
    files: usize,
    directories: usize,
    found: BTreeMap<String, OnDisk>,
}

impl Walk<'_> {
    /// Takes `entry` of `dir` (`""` for the root): records it, skips it, or returns the
    /// directory to enter.
    fn visit(&mut self, dir: &str, entry: &DirEntry) -> Result<Option<String>, Stop> {
        let at_root = dir.is_empty();
        let path = child(dir, entry.file_name())?;
        if at_root && path == GIT_DIR {
            return Ok(None);
        }
        let kind = entry.file_type().map_err(read_error(&entry.path()))?;
        let submodule = self.submodules.contains(path.as_str());
        if kind.is_dir() && !submodule {
            self.directories = self.directories.saturating_add(1);
            self.within_caps()?;
            let skipped = at_root && path == TARGET_DIR && !self.tracks_target;
            return Ok((!skipped).then_some(path));
        }
        self.files = self.files.saturating_add(1);
        self.within_caps()?;
        let on_disk = if kind.is_dir() {
            submodule_dir(&self.checkout.join(&path))?
        } else {
            on_disk(&entry.metadata().map_err(read_error(&entry.path()))?)
        };
        match self.found.entry(path) {
            Entry::Vacant(slot) => {
                slot.insert(on_disk);
                Ok(None)
            }
            // Two names that read as one path: the tree cannot hold both. The error owns the
            // path; the map keeps its key.
            Entry::Occupied(taken) => Err(Stop::Differs(Mismatch {
                path: taken.key().clone(),
                difference: Difference::Untracked,
            })),
        }
    }

    /// Refuses a walk past either cap.
    fn within_caps(&self) -> Result<(), PinnedError> {
        if self.files > self.caps.files || self.directories > self.caps.directories {
            return Err(PinnedError::CheckoutTooLarge {
                files: self.caps.files,
                directories: self.caps.directories,
            });
        }
        Ok(())
    }
}

/// The `/`-separated path of `name` in `dir`; a name that is not UTF-8 is never the tree's, so
/// it is untracked, named by its lossy text.
fn child(dir: &str, name: OsString) -> Result<String, Stop> {
    let join = |name: &str| {
        if dir.is_empty() {
            name.to_owned()
        } else {
            format!("{dir}/{name}")
        }
    };
    name.into_string().map(|name| join(&name)).map_err(|name| {
        Stop::Differs(Mismatch {
            path: join(&name.to_string_lossy()),
            difference: Difference::Untracked,
        })
    })
}

/// A submodule's directory: empty or not, read no further than its first entry.
fn submodule_dir(path: &Path) -> Result<OnDisk, PinnedError> {
    match fs::read_dir(path).map_err(read_error(path))?.next() {
        None => Ok(OnDisk::EmptyDir),
        Some(Ok(_)) => Ok(OnDisk::Dir),
        Some(Err(source)) => Err(read_error(path)(source)),
    }
}

/// [`PinnedError::CheckoutRead`] at `path`.
fn read_error(path: &Path) -> impl Fn(io::Error) -> PinnedError + '_ {
    move |source| PinnedError::CheckoutRead {
        path: path.to_path_buf(),
        source,
    }
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
