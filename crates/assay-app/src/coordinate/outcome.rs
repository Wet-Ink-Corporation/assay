//! What the coordinator stage produces: the kept findings ranked and grouped, the duplicates
//! merged away, how it ran, what it spent, and the events and `decide` outcome that follow
//! (04 §4 step 7, 03 §6).

use std::fmt;

use assay_domain::{Aeu, CallFailure, Disposition, EventPayload, Fingerprint, SupportOutcome};

use super::answer::CoordinationRefusal;
use crate::model_runner::RateLimitObserved;
use crate::validation::Placement;

/// A root-cause group: `g1`, `g2` and so on, numbered in rank order, so no two groups of a run
/// share an id. `decide` counts majors by distinct group (04 §5 rule 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupId(usize);

impl GroupId {
    /// The group ranked `position`th, counting from zero.
    pub(super) const fn at(position: usize) -> Self {
        Self(position.saturating_add(1))
    }
}

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "g{}", self.0)
    }
}

/// A finding the stage kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ranked {
    /// Its position in the findings given.
    pub index: usize,
    /// Its root-cause group.
    pub group: GroupId,
    /// Where it is published: as validation placed it, except that a finding the coordinator
    /// marked as noise moves to the summary.
    pub placement: Placement,
}

/// A finding merged into another as its duplicate (`duplicate`, 03 §5): not published, not
/// verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Merged {
    /// Its position in the findings given.
    pub index: usize,
    /// The position of the kept finding it duplicates.
    pub into: usize,
    /// Its own fingerprint (not the keeper's, which a coordinator merge may not share), which
    /// its `duplicate` event names (#263).
    pub fp: Fingerprint,
}

/// Who grouped the findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergedBy {
    /// The coordinator's answer, accepted.
    Coordinator,
    /// The deterministic merge: no call was due, or the coordinator failed.
    Mechanical,
}

/// Why the coordinator's answer was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinatorFailure<E> {
    /// The call ended without an accepted answer, or the stage budget was spent before it.
    Call(CallFailure),
    /// The runner could not make or supervise the call; it is charged at its budget.
    Runner(E),
    /// More distinct findings than [`MAX_COORDINATED_FINDINGS`](super::MAX_COORDINATED_FINDINGS):
    /// no call was made.
    TooMany {
        /// How many distinct findings there were.
        findings: usize,
    },
}

impl<E> CoordinatorFailure<E> {
    /// The degraded reason `decide` reads (04 §5 rules 8b, 8c). Too many findings is the
    /// coordinator's cap: more than one call can take is never attempted.
    #[must_use]
    pub const fn reason(&self) -> CallFailure {
        match self {
            Self::Call(reason) => *reason,
            Self::Runner(_) => CallFailure::Unavailable,
            Self::TooMany { .. } => CallFailure::Cap,
        }
    }
}

/// The outcome of the coordinator stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordination<E> {
    /// The findings kept, in rank order: severity, then confidence, then the coordinator's
    /// order (input order when merged mechanically), noise last. Publication posts the first
    /// [`MAX_INLINE_COMMENTS`](crate::MAX_INLINE_COMMENTS) placed inline.
    pub ranked: Vec<Ranked>,
    /// The findings merged as duplicates, in input order.
    pub merged: Vec<Merged>,
    /// Who grouped them.
    pub merged_by: MergedBy,
    /// Why the coordinator's answer was not used, if a call was due and it was not.
    pub failure: Option<CoordinatorFailure<E>>,
    /// Model calls made, the retry included.
    pub attempts: u32,
    /// What every attempt cost, to ledger.
    pub cost: Aeu,
    /// The rate-limit events of every attempt, in order.
    pub rate_limits: Vec<RateLimitObserved>,
    /// Why each refused answer was refused, in order (at most two).
    pub refusals: Vec<CoordinationRefusal>,
}

impl<E> Coordination<E> {
    /// How the coordinator ran, for `decide`: failed if its answer was not used, completed if it
    /// was, otherwise not needed (no findings, or a tier without a coordinator).
    #[must_use]
    pub const fn outcome(&self) -> SupportOutcome {
        match (&self.failure, self.merged_by) {
            (Some(failure), _) => SupportOutcome::Failed {
                reason: failure.reason(),
            },
            (None, MergedBy::Coordinator) => SupportOutcome::Completed,
            (None, MergedBy::Mechanical) => SupportOutcome::NotNeeded,
        }
    }

    /// The stage's events: a `FindingDisposed` (`duplicate`) per merged finding, in input order;
    /// then `CoordinatorFinished` when the coordinator's answer was used, or a `StageDegraded`
    /// for the coordinator when it failed.
    pub fn events(&self) -> impl Iterator<Item = EventPayload> + '_ {
        let merged = self
            .merged
            .iter()
            .map(|merge| EventPayload::FindingDisposed {
                code: Disposition::Duplicate,
                fp: Some(merge.fp),
                from: None,
                to: None,
            });
        let finished = (self.merged_by == MergedBy::Coordinator)
            .then_some(EventPayload::CoordinatorFinished {});
        let degraded = self
            .failure
            .as_ref()
            .map(|failure| EventPayload::StageDegraded {
                stage: "coordinator".to_owned(),
                reason: failure.reason().code().to_owned(),
            });
        merged.chain(finished).chain(degraded)
    }
}
