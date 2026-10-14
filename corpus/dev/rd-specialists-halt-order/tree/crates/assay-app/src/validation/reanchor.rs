//! Step 2's evidence search, shared with the re-review's anchor re-check of carried findings
//! (`docs/architecture/04-pipeline.md` §7): the same rule decides where a finding's evidence is,
//! whether it was raised in this run or carried from an earlier one.

use std::collections::HashMap;
use std::num::NonZeroU64;

use assay_domain::{Side, normalise};

use super::change::Change;
use super::evidence::{self, Joined, Match};

/// Whether `c` is a character the injection screen reads as invisible (zero-width, direction,
/// variation selector, tag or another format character).
pub(crate) fn is_invisible(c: char) -> bool {
    super::screen::is_invisible(c)
}

/// Whether `evidence` has enough content for step 2 to search for it.
pub(crate) fn is_searchable_evidence(evidence: &str) -> bool {
    evidence::has_enough_content(evidence)
}

/// `file`'s path at head, following a rename the change records, and its text there with the
/// key the change holds it under (borrowed, so a memo needs no copy of the path); no text when
/// the file is gone at head or its text was not supplied.
pub(crate) fn head_file<'c>(
    change: &'c Change,
    file: &str,
) -> (String, Option<(&'c str, &'c str)>) {
    let located = change.locate(file, Side::Right);
    let text = located
        .text
        .filter(|text| text.side == Side::Right)
        .map(|text| (text.path, text.content));
    (located.file_at_head, text)
}

/// A file's text indexed for step 2's evidence search: built once, searched once per distinct
/// normalised evidence, as validation's own index is.
#[derive(Debug)]
pub(crate) struct EvidenceIndex {
    joined: Joined,
    found: HashMap<String, Vec<Match>>,
    /// How many times the joined text was searched.
    #[cfg(test)]
    searched: usize,
}

impl EvidenceIndex {
    /// Indexes `text`, linear in its length.
    pub(crate) fn of(text: &str) -> Self {
        Self {
            joined: Joined::of(text),
            found: HashMap::new(),
            #[cfg(test)]
            searched: 0,
        }
    }

    /// The line nearest `cited` where `evidence` starts, as step 2 relocates a finding (the
    /// earlier on a tie); `None` when it occurs nowhere. Evidence that normalises alike is
    /// searched for once.
    pub(crate) fn nearest(&mut self, evidence: &str, cited: NonZeroU64) -> Option<NonZeroU64> {
        let normalised = normalise(evidence);
        let matches = self
            .found
            .entry(normalised)
            .or_insert_with_key(|normalised| {
                #[cfg(test)]
                {
                    self.searched = self.searched.saturating_add(1);
                }
                self.joined.find_normalised(normalised)
            });
        evidence::nearest(matches, cited.get()).and_then(|found| NonZeroU64::new(found.line))
    }

    /// How many times the file was searched.
    #[cfg(test)]
    pub(crate) const fn searches(&self) -> usize {
        self.searched
    }
}
