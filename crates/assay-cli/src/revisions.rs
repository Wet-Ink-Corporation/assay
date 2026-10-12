//! `assay-forge`'s object-only repository, lent to the temper adapter as the PR's base and head
//! trees (`assay_gates::RevisionFiles`), which `assay-gates` may not read from forge itself
//! (ADR-07).

use assay_forge::git::{self, CommitSha, GitError, ObjectRepo, TreeLimits};
use assay_gates::{EntryKind, Revision, RevisionFiles, TreeEntry};

/// The PR's base and head commits in a hardened object-only repository, read as data.
///
/// [`ObjectRepo::list_tree`] and [`ObjectRepo::read_blobs`] check nothing out, apply no
/// in-tree attribute, filter or textconv, and bound every listing and read by the
/// [`TreeLimits`].
#[derive(Debug)]
pub struct ForgeRevisions<'repo> {
    repo: &'repo ObjectRepo,
    base: CommitSha,
    head: CommitSha,
    limits: TreeLimits,
}

impl<'repo> ForgeRevisions<'repo> {
    /// Reads `base` and `head`, both already fetched into `repo`, within `limits`.
    #[must_use]
    pub const fn new(
        repo: &'repo ObjectRepo,
        base: CommitSha,
        head: CommitSha,
        limits: TreeLimits,
    ) -> Self {
        Self {
            repo,
            base,
            head,
            limits,
        }
    }
}

impl RevisionFiles for ForgeRevisions<'_> {
    type Error = GitError;

    /// The entries at or under `path` in the base or head commit, or all of them. An
    /// executable is a [`EntryKind::File`].
    ///
    /// # Errors
    ///
    /// [`GitError`] when git cannot list the tree, or it holds more entries than the limits
    /// allow. A `ProcessError::Kill` inside it means a git process group may still be
    /// running: abort the job.
    fn list(&self, revision: Revision, path: Option<&str>) -> Result<Vec<TreeEntry>, GitError> {
        let commit = match revision {
            Revision::Base => &self.base,
            Revision::Head => &self.head,
        };
        let entries = self.repo.list_tree(commit, path, self.limits)?;
        Ok(entries
            .into_iter()
            .map(|entry| TreeEntry {
                path: entry.path,
                kind: match entry.kind {
                    git::EntryKind::File | git::EntryKind::Executable => EntryKind::File,
                    git::EntryKind::Symlink => EntryKind::Symlink,
                    git::EntryKind::Submodule => EntryKind::Submodule,
                },
                object: entry.object,
                size: entry.size,
            })
            .collect())
    }

    /// The blobs of `entries`, in one `git cat-file --batch`.
    ///
    /// # Errors
    ///
    /// [`GitError`] when an entry is a submodule, the entries hold more bytes than the limits
    /// allow, or git cannot read a blob at its listed size.
    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, GitError> {
        let listed: Vec<git::TreeEntry> = entries
            .iter()
            .map(|entry| git::TreeEntry {
                // Forge's entry owns its fields; these copies live for one read.
                path: entry.path.clone(),
                kind: match entry.kind {
                    EntryKind::File => git::EntryKind::File,
                    EntryKind::Symlink => git::EntryKind::Symlink,
                    EntryKind::Submodule => git::EntryKind::Submodule,
                },
                object: entry.object.clone(),
                size: entry.size,
            })
            .collect();
        let listed: Vec<&git::TreeEntry> = listed.iter().collect();
        self.repo.read_blobs(&listed, self.limits)
    }

    /// The blob id git gives `bytes`.
    ///
    /// # Errors
    ///
    /// [`GitError`] when `git hash-object` fails.
    fn blob_id(&self, bytes: &[u8]) -> Result<String, GitError> {
        self.repo.hash_blob(bytes)
    }
}
