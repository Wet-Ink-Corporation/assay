//! `assay-forge`'s object-only repository, lent to the temper adapter as the PR's base and head
//! files (`assay_gates::RevisionFiles`), which `assay-gates` may not read from forge itself
//! (ADR-07).

use assay_forge::git::{CommitSha, GitError, ObjectRepo, TreeContent};
use assay_gates::{FileContent, Revision, RevisionFiles, TreeFile};

/// The PR's base and head commits in a hardened object-only repository, read as data with
/// [`ObjectRepo::read_tree`]: nothing is checked out, and no in-tree attribute, filter or
/// textconv applies.
#[derive(Debug)]
pub struct ForgeRevisions<'repo> {
    repo: &'repo ObjectRepo,
    base: CommitSha,
    head: CommitSha,
}

impl<'repo> ForgeRevisions<'repo> {
    /// Reads `base` and `head`, both already fetched into `repo`.
    #[must_use]
    pub const fn new(repo: &'repo ObjectRepo, base: CommitSha, head: CommitSha) -> Self {
        Self { repo, base, head }
    }
}

impl RevisionFiles for ForgeRevisions<'_> {
    type Error = GitError;

    /// The files at or under `path` in the base or head commit. Regular and executable files
    /// carry their bytes; a symlink or a submodule is [`FileContent::NotRegular`].
    ///
    /// # Errors
    ///
    /// [`GitError`] when git cannot list the tree or read a blob. A `ProcessError::Kill` inside
    /// it means a git process group may still be running: abort the job.
    fn files(&self, revision: Revision, path: &str) -> Result<Vec<TreeFile>, GitError> {
        let commit = match revision {
            Revision::Base => &self.base,
            Revision::Head => &self.head,
        };
        let files = self.repo.read_tree(commit, path)?;
        Ok(files
            .into_iter()
            .map(|file| {
                let (path, content) = file.into_parts();
                let content = match content {
                    TreeContent::File(bytes) | TreeContent::Executable(bytes) => {
                        FileContent::Bytes(bytes)
                    }
                    TreeContent::Symlink | TreeContent::Submodule => FileContent::NotRegular,
                };
                TreeFile { path, content }
            })
            .collect())
    }
}
