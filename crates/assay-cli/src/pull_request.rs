//! `assay-forge`'s object-only repository, lent to the `plan` use case as the PR's objects
//! (`assay_app::PullRequestObjects`) and to `check_local` as the change's patches
//! (`assay_app::ChangePatches`).

use std::error::Error;
use std::fmt;
use std::time::Duration;

use assay_app::{
    ChangePatches, ChangedFiles, CommitId, EntryKind, FilePatch, Hunk, LineSpan, PatchBudget,
    PullRequestObjects, TreeEntry,
};
use assay_domain::{FileChange, FileKind, Lines};
use assay_forge::git::{
    self, ChangeListing, ChangedFile, CommitSha, GitError, LineCount, ObjectRepo, ParseShaError,
    TreeLimits,
};
use assay_forge::process::ProcessError;

/// The PR's commits in a hardened object-only repository, read as data: the file list over
/// `base...head` within a wall-clock cap, and listings and reads bounded by [`TreeLimits`].
#[derive(Debug)]
pub struct ForgePullRequest<'repo> {
    repo: &'repo ObjectRepo,
    limits: TreeLimits,
    diff_cap: Duration,
}

impl<'repo> ForgePullRequest<'repo> {
    /// Reads commits already fetched into `repo`, each listing and read within `limits`, each
    /// diff within `diff_cap`.
    #[must_use]
    pub const fn new(repo: &'repo ObjectRepo, limits: TreeLimits, diff_cap: Duration) -> Self {
        Self {
            repo,
            limits,
            diff_cap,
        }
    }
}

/// Why the PR's objects could not be read.
#[derive(Debug)]
pub enum ForgeObjectsError {
    /// git failed. A `ProcessError::Kill` inside it means a git process group may still be
    /// running: abort the job.
    Git(GitError),
    /// A commit id forge does not accept.
    Commit(ParseShaError),
}

impl fmt::Display for ForgeObjectsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(_) => f.write_str("git could not read the pull request's objects"),
            Self::Commit(_) => f.write_str("a commit id is not one git accepts"),
        }
    }
}

impl Error for ForgeObjectsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Git(source) => Some(source),
            Self::Commit(source) => Some(source),
        }
    }
}

impl From<GitError> for ForgeObjectsError {
    fn from(source: GitError) -> Self {
        Self::Git(source)
    }
}

fn sha(commit: &CommitId) -> Result<CommitSha, ForgeObjectsError> {
    commit.as_str().parse().map_err(ForgeObjectsError::Commit)
}

impl PullRequestObjects for ForgePullRequest<'_> {
    type Error = ForgeObjectsError;

    /// `git diff --raw` and `--numstat --text` over `base...head`.
    ///
    /// # Errors
    ///
    /// [`ForgeObjectsError`] when git cannot list the files.
    fn changed_files(
        &self,
        base: &CommitId,
        head: &CommitId,
    ) -> Result<ChangedFiles, ForgeObjectsError> {
        let listing = self
            .repo
            .changed_files(&sha(base)?, &sha(head)?, self.diff_cap)?;
        Ok(match listing {
            ChangeListing::Listed(files) => {
                ChangedFiles::Listed(files.into_iter().map(file_change).collect())
            }
            ChangeListing::NeedsSplit => ChangedFiles::TooSlow,
        })
    }

    /// The entries at or under `path` in `commit`, or all of them. An executable is a
    /// [`EntryKind::File`].
    ///
    /// # Errors
    ///
    /// [`ForgeObjectsError`] when git cannot list the tree, or it holds more entries than the
    /// limits allow.
    fn list(
        &self,
        commit: &CommitId,
        path: Option<&str>,
    ) -> Result<Vec<TreeEntry>, ForgeObjectsError> {
        let entries = self.repo.list_tree(&sha(commit)?, path, self.limits)?;
        Ok(entries
            .into_iter()
            .map(|entry| TreeEntry {
                path: entry.path,
                kind: entry_kind(entry.kind),
                object: entry.object,
                size: entry.size,
            })
            .collect())
    }

    /// The blobs of `entries`, in one `git cat-file --batch`.
    ///
    /// # Errors
    ///
    /// [`ForgeObjectsError`] when an entry is a submodule, the entries hold more bytes than
    /// the limits allow, or git cannot read a blob at its listed size.
    fn read(&self, entries: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, ForgeObjectsError> {
        let blobs: Vec<git::BlobRef<'_>> = entries
            .iter()
            .map(|entry| git::BlobRef {
                path: &entry.path,
                object: &entry.object,
                size: entry.size,
            })
            .collect();
        Ok(self.repo.read_blobs(&blobs, self.limits)?)
    }
}

impl ChangePatches for ForgePullRequest<'_> {
    type Error = ForgeObjectsError;

    /// One `git diff -M --unified=3 base...head` per file, over its path and, for a rename,
    /// its base path too. The text is read as UTF-8 with any invalid byte replaced: it is
    /// context for the model, while the hunks are git's own line numbers. Each text passes
    /// through `budget` before the next diff runs, so at most the budget's bytes of text, plus
    /// one diff, are held. A diff git cannot print within forge's output cap (64 MiB by
    /// default) is not shown and has no hunks, as for a binary file, rather than failing the
    /// whole change.
    ///
    /// # Errors
    ///
    /// [`ForgeObjectsError`] when a diff cannot run (other than over the output cap) or does
    /// not read as one file's patch.
    fn patches(
        &self,
        (base, head): (&CommitId, &CommitId),
        files: &[FileChange],
        mut budget: PatchBudget,
    ) -> Result<Vec<FilePatch>, ForgeObjectsError> {
        let (base, head) = (sha(base)?, sha(head)?);
        files
            .iter()
            .map(|file| {
                let paths: Vec<&str> = file
                    .renamed_from
                    .as_deref()
                    .into_iter()
                    .chain([file.path.as_str()])
                    .collect();
                let patch = match self.repo.file_patch(&base, &head, &paths) {
                    Ok(patch) => patch,
                    Err(error) if is_output_overrun(&error) => return Ok(FilePatch::default()),
                    Err(error) => return Err(error.into()),
                };
                let text = String::from_utf8_lossy(&patch.text).into_owned();
                Ok(FilePatch {
                    text: budget.keep(text),
                    hunks: patch
                        .hunks
                        .into_iter()
                        .map(|hunk| Hunk {
                            base: span(hunk.base),
                            head: span(hunk.head),
                        })
                        .collect(),
                })
            })
            .collect()
    }
}

/// Whether git was killed for printing more than forge's output cap and nothing else: the
/// group was shown gone, so the file can be left out. A failed group kill is not one, since a
/// git process may still be running (#219 R2).
const fn is_output_overrun(error: &GitError) -> bool {
    matches!(
        error,
        GitError::Process {
            source: ProcessError::OutputTooLarge,
            ..
        }
    )
}

const fn span(range: git::LineRange) -> LineSpan {
    LineSpan {
        start: range.start,
        count: range.count,
    }
}

fn entry_kind(kind: git::EntryKind) -> EntryKind {
    match kind {
        git::EntryKind::File | git::EntryKind::Executable => EntryKind::File,
        git::EntryKind::Symlink => EntryKind::Symlink,
        git::EntryKind::Submodule => EntryKind::Submodule,
    }
}

/// The domain's view of one changed file: a gitlink on either side is a `Gitlink`, else a
/// symlink on either side a `Symlink`; a file git did not measure on either count is
/// `Binary`.
fn file_change(file: ChangedFile) -> FileChange {
    let sides = [file.base_kind(), file.head_kind()];
    let kind = if sides.contains(&Some(git::EntryKind::Submodule)) {
        FileKind::Gitlink
    } else if sides.contains(&Some(git::EntryKind::Symlink)) {
        FileKind::Symlink
    } else {
        FileKind::File
    };
    let lines = match (file.added(), file.deleted()) {
        (LineCount::Counted(added), LineCount::Counted(removed)) => {
            Lines::Counted { added, removed }
        }
        (LineCount::Unmeasured, _) | (_, LineCount::Unmeasured) => Lines::Binary,
    };
    let (path, renamed_from) = file.into_paths();
    FileChange {
        path,
        renamed_from,
        lines,
        kind,
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use assay_forge::git::GitOperation;

    use super::*;

    fn process(source: ProcessError) -> GitError {
        GitError::Process {
            operation: GitOperation::Diff,
            source,
        }
    }

    #[test]
    fn only_an_output_overrun_whose_group_is_gone_leaves_a_file_out() {
        let failed_kill = ProcessError::Kill {
            cause: Box::new(ProcessError::OutputTooLarge),
            source: io::Error::other("kill failed"),
        };

        assert!(is_output_overrun(&process(ProcessError::OutputTooLarge)));
        assert!(
            !is_output_overrun(&process(failed_kill)),
            "a git process may still be running"
        );
        assert!(!is_output_overrun(&process(ProcessError::TimedOut)));
    }
}
