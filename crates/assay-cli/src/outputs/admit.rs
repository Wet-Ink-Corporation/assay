//! `admit`'s outputs (03 §4a, 06 §1, #60 ST-11): what the admission decided ([`AdmitOutputs`]),
//! and the review slot it holds for the run, which `release` releases ([`HeldSlot`]).

use assay_app::{Admission, Admitted, CommitId, Sha256Digest, Slot, SlotHold, TierDegrade};
use assay_domain::{Aeu, Tier};

use super::{Read, flag_word};
use crate::ci::{JobName, Needs, NeedsError, OutputName};

/// The job whose outputs [`HeldSlot`] is.
const ADMIT: JobName = JobName::Admit;

/// What `admit` reports of its admission, read off what it decided and wrote.
///
/// Built only from an admission ([`AdmitOutputs::new`]), so the slot, the reservation, the
/// review flag and the pack digest are those of a spend and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmitOutputs {
    admission: Admission,
    tier: Tier,
    degrade: TierDegrade,
    run_digest: Sha256Digest,
    pack_digest: Option<Sha256Digest>,
}

impl AdmitOutputs {
    /// The outputs of `admitted`, for a plan of tier `planned` (the tier a run that does not
    /// spend reports): `run_digest` is the sha256 of the admitted `run.json`, and `pack_digest`
    /// the context pack's.
    #[must_use]
    pub fn new(admitted: &Admitted, planned: Tier) -> Self {
        let tier = match admitted.admission() {
            Admission::Spend(spend) => spend.tier,
            Admission::NoSpend(_) | Admission::Reused(_) | Admission::Deferred(_) => planned,
        };
        Self {
            // The outputs keep the decision; the admission keeps it for its documents.
            admission: admitted.admission().clone(),
            tier,
            degrade: admitted.degrade(),
            run_digest: Sha256Digest::of(admitted.run_json()),
            // The pack keeps its digest beside its bytes; the outputs write their own copy.
            pack_digest: admitted.pack().map(|pack| pack.digest().clone()),
        }
    }

    /// The slot the run holds, for a spend.
    #[must_use]
    pub fn held(&self) -> Option<HeldSlot> {
        match &self.admission {
            Admission::Spend(spend) => Some(HeldSlot::from(&spend.hold)),
            Admission::NoSpend(_) | Admission::Reused(_) | Admission::Deferred(_) => None,
        }
    }

    /// The sha256 of the admitted `run.json`.
    #[must_use]
    pub const fn run_digest(&self) -> &Sha256Digest {
        &self.run_digest
    }

    /// The sha256 of `context-pack.json`, written for a spend only.
    #[must_use]
    pub const fn pack_digest(&self) -> Option<&Sha256Digest> {
        self.pack_digest.as_ref()
    }

    /// The `admission` output's word.
    #[must_use]
    pub const fn admission(&self) -> &'static str {
        match self.admission {
            Admission::NoSpend(_) => "no-spend",
            Admission::Reused(_) => "reused",
            Admission::Deferred(_) => "deferred",
            Admission::Spend(_) => "spend",
        }
    }

    /// admit's outputs, in [`OutputName::ADMIT`]'s order: `admission`; `deferral` for a
    /// deferral only (`budget-deferred` or `slot-busy`); `slot` and `lease_commit` for a spend
    /// only; `reserved_aeu` (`0` unless a spend reserved); `tier` (the tier the LLM stage runs
    /// at for a spend, else the plan's); `degrade` (`full` after a full-to-lite degrade, else
    /// `none`); `run_digest`; `pack_digest` for a spend only; and `review` (`true` for a spend
    /// only).
    #[must_use]
    pub fn outputs(&self) -> Vec<(OutputName, String)> {
        let mut outputs = vec![(OutputName::Admission, self.admission().to_owned())];
        if let Admission::Deferred(deferred) = self.admission {
            let code = deferred.deferral().code();
            outputs.push((OutputName::Deferral, code.to_owned()));
        }
        if let Some(held) = self.held() {
            outputs.extend(held.outputs());
        }
        let reserved = match &self.admission {
            Admission::Spend(spend) => spend.reserved,
            Admission::NoSpend(_) | Admission::Reused(_) | Admission::Deferred(_) => {
                Aeu::from_micros(0)
            }
        };
        let degrade = match self.degrade {
            TierDegrade::None => "none",
            TierDegrade::FullToLite => Tier::Full.code(),
        };
        outputs.extend([
            (OutputName::ReservedAeu, reserved.to_string()),
            (OutputName::Tier, self.tier.code().to_owned()),
            (OutputName::Degrade, degrade.to_owned()),
            (OutputName::RunDigest, self.run_digest.to_string()),
        ]);
        if let Some(pack) = &self.pack_digest {
            outputs.push((OutputName::PackDigest, pack.to_string()));
        }
        let review = matches!(self.admission, Admission::Spend(_));
        outputs.push((OutputName::Review, flag_word(review).to_owned()));
        outputs
    }
}

/// The review slot `admit` holds for the run: its index and the commit its ref points at, which
/// releasing compares against (A-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSlot {
    /// The slot.
    pub slot: Slot,
    /// The lease's commit.
    pub lease_commit: CommitId,
}

impl From<&SlotHold> for HeldSlot {
    /// The slot `hold` took and the commit its ref points at.
    fn from(hold: &SlotHold) -> Self {
        Self {
            slot: hold.slot,
            // The outputs own the commit they write; the hold keeps its own.
            lease_commit: hold.lease_commit.clone(),
        }
    }
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
