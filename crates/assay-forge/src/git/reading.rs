//! [`ObjectRepo::reading_checkout`]: a checkout of a fetched commit for a pinned tool that reads
//! a repository rather than blobs (`weighin ci-check`, #372).
//!
//! The checkout is a non-bare repository in `dest/.git` that borrows the object-only
//! repository's store through `objects/info/alternates`, so the fetched history (the base
//! included) is reachable from it while every object stays where it was fetched. Its `HEAD` is
//! detached at the commit, and its index and files are written by the hardened git as
//! [`ObjectRepo::checkout`] writes a snapshot's: no hook, filter or attribute applies, and every
//! symlink is a plain file holding its target, so nothing read from `dest` follows a link out
//! of it. The files are the PR's and stay data: assay runs nothing from them.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{self, Path};

use super::GitError;
use super::command::{GitOperation, Scope};
use super::repository::{Call, ObjectRepo};
use super::sha::CommitSha;
use super::worktree::{alternates_line, empty_destination};

impl ObjectRepo {
    /// Writes a reading checkout of `commit` (fetched into this repository) into `dest`, which
    /// must be absent or empty: a repository at `dest/.git` whose objects are this
    /// repository's, `HEAD` detached at `commit`, and its files written with every symlink as
    /// a plain file.
    ///
    /// # Errors
    /// [`GitError::DirectoryNotEmpty`] when `dest` has contents, [`GitError::Inspect`] when it
    /// cannot be read or created or a path cannot be made absolute, [`GitError::Write`] when
    /// the alternates file cannot be written (or this repository's path holds a newline), and
    /// [`GitError`] when a git step cannot run or fails (a commit this repository does not
    /// hold included).
    pub fn reading_checkout(&self, commit: &CommitSha, dest: &Path) -> Result<(), GitError> {
        let inspect = |path: &Path| {
            let git_dir = path.to_path_buf();
            move |source| GitError::Inspect { git_dir, source }
        };
        let dest = path::absolute(dest).map_err(inspect(dest))?;
        empty_destination(&dest)?;
        let git_dir = dest.join(".git");
        let scope = Scope {
            work_tree: Some(&dest),
            ..Scope::default()
        };
        self.run_in_repo(
            &git_dir,
            Call {
                scope,
                ..self.call(GitOperation::InitReading, &[])
            },
        )?;
        let objects = self.git_dir().join("objects");
        let objects = path::absolute(&objects).map_err(inspect(&objects))?;
        borrow_store(&git_dir, &objects)?;
        let sha = [commit.as_str()];
        self.run_in_repo(&git_dir, self.call(GitOperation::DetachHead, &sha))?;
        self.run_in_repo(
            &git_dir,
            Call {
                scope,
                ..self.call(GitOperation::ReadTree, &sha)
            },
        )?;
        self.run_in_repo(
            &git_dir,
            Call {
                scope,
                ..self.call(GitOperation::CheckoutIndex, &[])
            },
        )?;
        Ok(())
    }
}

/// Points the repository at `git_dir` at the object store `objects` (absolute) through a new
/// `objects/info/alternates`.
fn borrow_store(git_dir: &Path, objects: &Path) -> Result<(), GitError> {
    let path = git_dir.join("objects/info/alternates");
    let Some(line) = alternates_line(objects) else {
        return Err(GitError::Write {
            path,
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "the object store's path holds a newline",
            ),
        });
    };
    let written = fs::create_dir_all(git_dir.join("objects/info")).and_then(|()| {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(&line))
    });
    written.map_err(|source| GitError::Write { path, source })
}
