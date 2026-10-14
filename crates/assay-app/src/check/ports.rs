//! The one port local mode adds to those `plan`, `gate` and the model runner define: the
//! change's unified diff, file by file.

use assay_domain::FileChange;

use crate::config::CommitId;
use crate::validation::Hunk;

/// One changed file's unified diff and its hunks, as git prints them over `base...head` with
/// rename detection and three lines of context (what GitHub shows of a PR).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilePatch {
    /// The diff text, for the review's context pack: empty for an unchanged file, `None` when
    /// it did not fit the [`PatchBudget`] and is not shown.
    pub text: Option<String>,
    /// Its `@@ -base +head @@` hunks, for validation's placement; none for a binary file. Kept
    /// for every file, shown or not, except one whose diff is over the adapter's output cap: it
    /// has no text and no hunks, like a binary file.
    pub hunks: Vec<Hunk>,
}

/// How many bytes of patch text a [`ChangePatches`] reply may still hold, so the patches of a
/// large change are never all in memory at once (H-13).
///
/// A text that would cross what is left is withheld and later, smaller ones may still fit: the
/// rule the context pack applies to the diff, so a text kept here is one the pack can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchBudget {
    left: usize,
}

impl PatchBudget {
    /// A budget of `bytes`.
    #[must_use]
    pub const fn new(bytes: usize) -> Self {
        Self { left: bytes }
    }

    /// `text` when it fits what is left, which it then uses up; `None` otherwise.
    pub fn keep(&mut self, text: String) -> Option<String> {
        let left = self.left.checked_sub(text.len())?;
        self.left = left;
        Some(text)
    }
}

/// The change's patches, read as data (nothing is checked out or run).
pub trait ChangePatches {
    /// Why the patches could not be read.
    type Error: std::error::Error;

    /// The patch of each of `files` over `base...head` (`commits`), in the same order: a renamed file's
    /// covers both its paths. Each text passes through `budget` as it is read
    /// ([`PatchBudget::keep`]), so the reply holds at most the budget's bytes of text; the hunks
    /// of every file are kept, except a file whose diff is over the adapter's output cap, which
    /// has no text and no hunks, like a binary file.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when a diff cannot be produced or read.
    fn patches(
        &self,
        commits: (&CommitId, &CommitId),
        files: &[FileChange],
        budget: PatchBudget,
    ) -> Result<Vec<FilePatch>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_is_kept_up_to_the_budget_and_a_later_smaller_one_still_fits() {
        let mut budget = PatchBudget::new(5);

        let first = budget.keep("abc".to_owned());
        let over = budget.keep("abc".to_owned());
        let fits = budget.keep("ab".to_owned());
        let spent = budget.keep("a".to_owned());

        assert_eq!(first.as_deref(), Some("abc"));
        assert_eq!(over, None, "one byte past what is left is withheld");
        assert_eq!(fits.as_deref(), Some("ab"), "exactly what is left fits");
        assert_eq!(spent, None);
        assert_eq!(budget.keep(String::new()).as_deref(), Some(""));
    }
}
