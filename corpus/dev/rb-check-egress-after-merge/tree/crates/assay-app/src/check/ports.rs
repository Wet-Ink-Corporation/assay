//! The one port local mode adds to those `plan`, `gate` and the model runner define: the
//! change's unified diff, file by file.

use assay_domain::FileChange;

use crate::config::CommitId;
use crate::validation::Hunk;

/// One changed file's unified diff and its hunks, as git prints them over `base...head` with
/// rename detection and three lines of context (what GitHub shows of a PR).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilePatch {
    /// The diff text, for the review's context pack; empty for an unchanged file.
    pub text: String,
    /// Its `@@ -base +head @@` hunks, for validation's placement; none for a binary file.
    pub hunks: Vec<Hunk>,
}

/// The change's patches, read as data (nothing is checked out or run).
pub trait ChangePatches {
    /// Why the patches could not be read.
    type Error: std::error::Error;

    /// The patch of each of `files` over `base...head`, in the same order: a renamed file's
    /// covers both its paths.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when a diff cannot be produced or read.
    fn patches(
        &self,
        base: &CommitId,
        head: &CommitId,
        files: &[FileChange],
    ) -> Result<Vec<FilePatch>, Self::Error>;
}
