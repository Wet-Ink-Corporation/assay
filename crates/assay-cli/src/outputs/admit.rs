//! `admit`'s `slot` and `lease_commit` outputs (03 §4a, 06 §1): the review slot it holds for
//! the run, which `release` releases.

use assay_app::{CommitId, Slot};

use super::Read;
use crate::ci::{JobName, Needs, NeedsError, OutputName};

/// The job whose outputs [`HeldSlot`] is.
const ADMIT: JobName = JobName::Admit;

/// The review slot `admit` holds for the run: its index and the commit its ref points at, which
/// releasing compares against (A-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSlot {
    /// The slot.
    pub slot: Slot,
    /// The lease's commit.
    pub lease_commit: CommitId,
}

impl HeldSlot {
    /// `admit`'s `slot` and `lease_commit` outputs for this slot, in that order.
    #[must_use]
    pub fn outputs(&self) -> [(OutputName, String); 2] {
        [
            (OutputName::Slot, self.slot.to_string()),
            (OutputName::LeaseCommit, self.lease_commit.to_string()),
        ]
    }

    /// The slot `admit` holds, read from `needs`; `None` when it wrote no `slot` (an empty
    /// value reads as not written).
    ///
    /// It is read whatever `admit` ended with: `admit` writes its outputs once it holds the slot
    /// and has written the reservation, so a job that failed after them still holds a slot to
    /// release.
    ///
    /// # Errors
    ///
    /// [`NeedsError::MissingJob`] without `admit`; [`NeedsError::UnexpectedOutput`] for an
    /// output that is not admit's; [`NeedsError::MissingOutput`] for a `slot` without a
    /// `lease_commit`; [`NeedsError::Output`] for a `slot` that is not a slot's index (`0` or
    /// `1`), a `lease_commit` that is not 40 lowercase hex digits, or a `lease_commit` without a
    /// `slot`.
    pub fn from_needs(needs: &Needs) -> Result<Option<Self>, NeedsError> {
        let read = Read::any_result(needs, ADMIT, &OutputName::ADMIT)?;
        if read.optional(OutputName::Slot).is_none() {
            return match read.optional(OutputName::LeaseCommit) {
                Some(_) => Err(read.invalid(OutputName::LeaseCommit)),
                None => Ok(None),
            };
        }
        let slot = read.parsed(OutputName::Slot, |text| {
            Slot::ALL
                .into_iter()
                .find(|known| known.to_string() == text)
        })?;
        let lease_commit = read.parsed(OutputName::LeaseCommit, |text| text.parse().ok())?;
        Ok(Some(Self { slot, lease_commit }))
    }
}

#[cfg(test)]
mod tests;
