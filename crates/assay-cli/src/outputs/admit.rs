//! `admit`'s `slot` and `lease_commit` outputs (03 §4a, 06 §1): the review slot it holds for
//! the run, which `release` releases.

use assay_app::{CommitId, Slot};

use super::{only_own, written};
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
        let admit = needs
            .job(ADMIT)
            .ok_or(NeedsError::MissingJob { job: ADMIT })?;
        only_own(admit, ADMIT, &OutputName::ADMIT)?;
        let lease = written(admit, OutputName::LeaseCommit);
        let Some(slot) = written(admit, OutputName::Slot) else {
            return match lease {
                Some(_) => Err(invalid(OutputName::LeaseCommit)),
                None => Ok(None),
            };
        };
        let slot = Slot::ALL
            .into_iter()
            .find(|known| known.to_string() == slot)
            .ok_or_else(|| invalid(OutputName::Slot))?;
        let lease_commit = lease
            .ok_or(NeedsError::MissingOutput {
                job: ADMIT,
                name: OutputName::LeaseCommit,
            })?
            .parse()
            .map_err(|_not_a_commit| invalid(OutputName::LeaseCommit))?;
        Ok(Some(Self { slot, lease_commit }))
    }
}

/// The refusal of `admit`'s output `name`'s value.
const fn invalid(name: OutputName) -> NeedsError {
    NeedsError::Output { job: ADMIT, name }
}

#[cfg(test)]
mod tests;
