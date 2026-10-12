//! The files of a PR's base and head commits, read as data (`docs/architecture/03-contracts.md`
//! §2.1, §9).
//!
//! The temper adapter reads `.claude/temper-baseline.json`, `.claude/temper.json`,
//! `.assay/exceptions.toml` and `.claude/temper/` through [`RevisionFiles`]. The production
//! implementation is `assay_cli::ForgeRevisions`, over `assay-forge`'s hardened object-only
//! repository; this crate does not depend on forge (ADR-07).

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

/// What a tree entry holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// A regular file (executable or not) and its bytes.
    Bytes(Vec<u8>),
    /// A symlink or a submodule: never read or followed.
    NotRegular,
}

/// One file of a commit's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFile {
    /// Its path from the root of the tree, `/`-separated.
    pub path: String,
    /// What it holds.
    pub content: FileContent,
}

/// Reads the files of the PR's base and head commits.
pub trait RevisionFiles {
    /// Why the files could not be read. `'static` because it becomes the `source` of an error.
    type Error: std::error::Error + 'static;

    /// The files at or under `path` in `revision`'s tree: the one file when `path` names a
    /// file, every file below it when it names a directory, none when nothing is there.
    /// `path` is literal (no wildcard), and `a` never matches `a.json` or `ab/c`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when the tree or a file cannot be read.
    fn files(&self, revision: Revision, path: &str) -> Result<Vec<TreeFile>, Self::Error>;
}

impl<R: RevisionFiles + ?Sized> RevisionFiles for &R {
    type Error = R::Error;

    fn files(&self, revision: Revision, path: &str) -> Result<Vec<TreeFile>, Self::Error> {
        (**self).files(revision, path)
    }
}
