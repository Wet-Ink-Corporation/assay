//! The port `plan` reads the PR through: its git objects, fetched as data.

use assay_domain::FileChange;

use crate::config::CommitId;

/// The PR's commits as data: the file list between them, and their trees and blobs. Nothing
/// is checked out and nothing in the trees runs (`docs/architecture/03-contracts.md` §9).
pub trait PullRequestObjects {
    /// Why the objects could not be read.
    type Error: std::error::Error;

    /// The files changed from the merge base of `base` and `head` to `head` (`git diff
    /// base...head`, as GitHub lists them), with both sides of each rename, what git records
    /// at each path and the line counts of `--numstat --text`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the list could not be produced; running past the diff's cap is
    /// [`ChangedFiles::TooSlow`] instead.
    fn changed_files(&self, base: &CommitId, head: &CommitId) -> Result<ChangedFiles, Self::Error>;

    /// The entries at or under `path` (whole path components, no wildcard) in `commit`'s tree,
    /// or all of them for `None`, in path order. Nothing is read.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the tree cannot be listed or holds more entries than the adapter
    /// reads.
    fn list(&self, commit: &CommitId, path: Option<&str>) -> Result<Vec<TreeEntry>, Self::Error>;

    /// The blobs of `entries`, in order; never asked for a submodule.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when a blob cannot be read, or the entries hold more bytes than the
    /// adapter reads.
    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, Self::Error>;
}

/// The PR's changed files, or none when git could not list them in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedFiles {
    /// Every changed file, in git's order.
    Listed(Vec<FileChange>),
    /// The diff ran past its cap (rename detection is quadratic): there is no list, and the
    /// PR needs a split.
    TooSlow,
}

/// What a tree entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A regular or executable file.
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
    /// The adapter's name for its object, handed back to [`PullRequestObjects::read`].
    pub object: String,
    /// Its blob's size in bytes; `None` for a submodule.
    pub size: Option<u64>,
}
