//! The trees of a PR's base and head commits, read as data (`docs/architecture/03-contracts.md`
//! §2.1, §9).
//!
//! The temper adapter reads `.claude/temper-baseline.json`, `.claude/temper.json`,
//! `.assay/exceptions.toml`, `.claude/temper/` and the head's Rust sources through
//! [`RevisionFiles`]. The production implementation is `assay_cli::ForgeRevisions`, over
//! `assay-forge`'s hardened object-only repository; this crate does not depend on forge
//! (ADR-07).

use std::fmt;

/// One side of the PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Revision {
    /// The base commit: trusted configuration is read only from here.
    Base,
    /// The head commit: the PR's proposal.
    Head,
}

impl fmt::Display for Revision {
    /// `base` or `head`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Base => "base",
            Self::Head => "head",
        })
    }
}

/// What a tree entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A regular file, executable or not.
    File,
    /// A symbolic link; its blob is the target's text, never followed.
    Symlink,
    /// A submodule: a commit of another repository, with no blob.
    Submodule,
}

/// One entry of a commit's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Its path from the root of the tree, `/`-separated.
    pub path: String,
    /// What it is.
    pub kind: EntryKind,
    /// Its full object id: equal ids mean equal bytes.
    pub object: String,
    /// Its blob's size in bytes; `None` for a submodule.
    pub size: Option<u64>,
}

/// Reads the trees of the PR's base and head commits. Implementations bound what one listing
/// and one read may hold (H-13) and fail rather than truncate.
pub trait RevisionFiles {
    /// Why the tree could not be read. `'static` because it becomes the `source` of an error.
    type Error: std::error::Error + 'static;

    /// The entries at or under `path` in `revision`'s tree, or every entry for `None`: the
    /// one entry when `path` names a file, every entry below it when it names a directory,
    /// none when nothing is there. `path` is literal (no wildcard), and `a` never matches
    /// `a.json` or `ab/c`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when the tree cannot be listed or holds more entries than the
    /// implementation allows.
    fn list(&self, revision: Revision, path: Option<&str>) -> Result<Vec<TreeEntry>, Self::Error>;

    /// The bytes of each of `entries` (files and symlinks from [`Self::list`]), in order.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when a blob cannot be read, an entry is a submodule, or the
    /// entries hold more bytes than the implementation allows.
    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, Self::Error>;

    /// The object id `bytes` would have as a blob, comparable with [`TreeEntry::object`].
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when the id cannot be computed.
    fn blob_id(&self, bytes: &[u8]) -> Result<String, Self::Error>;
}

impl<R: RevisionFiles + ?Sized> RevisionFiles for &R {
    type Error = R::Error;

    fn list(&self, revision: Revision, path: Option<&str>) -> Result<Vec<TreeEntry>, Self::Error> {
        (**self).list(revision, path)
    }

    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, Self::Error> {
        (**self).read(entries)
    }

    fn blob_id(&self, bytes: &[u8]) -> Result<String, Self::Error> {
        (**self).blob_id(bytes)
    }
}
