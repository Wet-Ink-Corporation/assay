//! Step 2's evidence search, shared with the re-review's anchor re-check of carried findings
//! (`docs/architecture/04-pipeline.md` §7): the same rule decides where a finding's evidence is,
//! whether it was raised in this run or carried from an earlier one.

use std::num::NonZeroU64;

use assay_domain::{Side, normalise};

use super::change::Change;
use super::evidence::{self, Joined};

/// Whether `c` is a character the injection screen reads as invisible (zero-width, direction,
/// variation selector, tag or another format character).
pub(crate) fn is_invisible(c: char) -> bool {
    super::screen::is_invisible(c)
}

/// Whether `evidence` has enough content for step 2 to search for it.
pub(crate) fn is_searchable_evidence(evidence: &str) -> bool {
    evidence::has_enough_content(evidence)
}

/// `file`'s path and text at head, following a rename the change records; no text when the
/// file is gone at head or its text was not supplied.
pub(crate) fn head_file<'c>(change: &'c Change, file: &str) -> (String, Option<&'c str>) {
    let located = change.locate(file, Side::Right);
    let text = located
        .text
        .filter(|text| text.side == Side::Right)
        .map(|text| text.content);
    (located.file_at_head, text)
}

/// A file's text indexed for step 2's evidence search: built once, searched for many findings.
#[derive(Debug)]
pub(crate) struct EvidenceIndex(Joined);

impl EvidenceIndex {
    /// Indexes `text`, linear in its length.
    pub(crate) fn of(text: &str) -> Self {
        Self(Joined::of(text))
    }

    /// The line nearest `cited` where `evidence` starts, as step 2 relocates a finding (the
    /// earlier on a tie); `None` when it occurs nowhere.
    pub(crate) fn nearest(&self, evidence: &str, cited: NonZeroU64) -> Option<NonZeroU64> {
        let matches = self.0.find_normalised(&normalise(evidence));
        evidence::nearest(&matches, cited.get()).and_then(|found| NonZeroU64::new(found.line))
    }
}
