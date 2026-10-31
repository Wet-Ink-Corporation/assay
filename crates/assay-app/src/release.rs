//! The `release` use case (S10, `docs/architecture/04-pipeline.md` §1, 06 §1, R3-06, R3-16):
//! settles an admitted run once its review ended.
//!
//! In order: refuse a re-run (R3-05); read the run's `reserved` cost record, which `admit` wrote
//! when it took the slot; release the slot by compare-and-set on the lease commit `admit`
//! reported, so a lease reaped since, and perhaps taken by a newer run, is never deleted; then
//! append the run's `final` record. Its fields are the reservation's, so the run's evidence key,
//! PR, head and tier have one source; its cost is what the review reported, or the run cap the
//! reservation holds when the review died or was cancelled (SIGTERM loses the CLI's cost, 03 §8),
//! with `cancelled` set only for a cancelled review.
//!
//! Every read happens before any write. A run whose `release` fails holds its slot until the
//! lease's TTL, after which `admit` or the ledger-side reconcile reaps it, and reconcile
//! back-fills the `final` record at the reservation (06 §1, F17).

use std::num::NonZeroU64;

use assay_domain::{Aeu, RunUid};
use thiserror::Error;

use crate::config::CommitId;
use crate::ledger::{
    Appended, CostKind, CostRecord, LeaseError, Ledger, LedgerReadError, LedgerRefs, LedgerTime,
    LedgerWriteError, Released, Slot, append_cost, read_costs, release_slot,
};

/// The ports `release` reads and writes, borrowed for one call.
#[derive(Debug)]
pub struct ReleasePorts<'p, L, R> {
    /// The ledger's files: the cost records.
    pub ledger: &'p L,
    /// The ledger's refs: the slot.
    pub refs: &'p R,
}

/// What `release` is given besides its ports.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseRequest<'a> {
    /// This workflow run, from the environment.
    pub run_uid: RunUid,
    /// The slot `admit` took.
    pub slot: Slot,
    /// The commit `admit`'s lease is, which the release compares against.
    pub lease_commit: &'a CommitId,
    /// How the review ended.
    pub review: ReviewCost,
    /// The time the `final` record is written at.
    pub now: LedgerTime,
}

/// How the review ended, as the `final` record charges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewCost {
    /// The review ended and reported what it spent.
    Reported(Aeu),
    /// The review failed or was skipped: it is charged the run cap.
    RunCap,
    /// The review was cancelled: it is charged the run cap, and the record says so.
    Cancelled,
}

/// What `release` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    /// Whether the slot still held the run's lease and was released, or had been reaped.
    pub slot: Released,
    /// The `final` record `release` built.
    pub record: CostRecord,
    /// Whether it was written, or a `final` record of the run was already there.
    pub cost: Appended<CostRecord>,
}

/// Why `release` failed. `L` and `R` are the ledger's and the refs' errors.
#[derive(Debug, Error)]
pub enum ReleaseError<L, R> {
    /// The run is a re-run: privileged jobs run on attempt 1 only (R3-05). Nothing was read.
    #[error("refused attempt {attempt}: release runs on attempt 1 only; start a fresh run")]
    NotFirstAttempt {
        /// The attempt.
        attempt: NonZeroU64,
    },
    /// The cost records could not be read. Nothing was written.
    #[error("the cost records could not be read")]
    Costs(#[source] LedgerReadError<L>),
    /// The cost records hold no `reserved` record of the run, so the `final` one has nothing to
    /// settle. Nothing was written.
    #[error(
        "the cost records hold no reservation of run {} attempt {} in repository {}",
        run_uid.run_id,
        run_uid.run_attempt,
        run_uid.repo_id
    )]
    NoReservation {
        /// The run.
        run_uid: RunUid,
    },
    /// The slot could not be released; no record was written.
    #[error("slot {slot} could not be released")]
    Slot {
        /// The slot.
        slot: Slot,
        /// Why.
        #[source]
        source: LeaseError<R>,
    },
    /// The slot was released or found reaped, but the `final` record could not be written; it may
    /// or may not have landed.
    #[error("the final cost record could not be written")]
    Final {
        /// What the slot release did.
        slot: Released,
        /// Why the write failed.
        #[source]
        source: LedgerWriteError<L>,
    },
}

/// The [`ReleaseError`] of [`release`] over the ports' error types.
pub type ReleaseFailure<L, R> = ReleaseError<<L as Ledger>::Error, <R as LedgerRefs>::Error>;

/// Settles the run: see the module documentation.
///
/// # Errors
///
/// [`ReleaseError::NotFirstAttempt`] before any port call; [`ReleaseError::Costs`] and
/// [`ReleaseError::NoReservation`] before any write; [`ReleaseError::Slot`] when the slot could
/// not be released, with no record written; [`ReleaseError::Final`] when the record could not
/// be.
pub fn release<L: Ledger, R: LedgerRefs>(
    ports: &ReleasePorts<'_, L, R>,
    request: &ReleaseRequest<'_>,
) -> Result<Settled, ReleaseFailure<L, R>> {
    let attempt = request.run_uid.run_attempt;
    if attempt.get() != 1 {
        return Err(ReleaseError::NotFirstAttempt { attempt });
    }
    let window = read_costs(ports.ledger, request.now).map_err(ReleaseError::Costs)?;
    let reserved = window
        .into_records()
        .into_iter()
        .find(|record| record.run_uid == request.run_uid && record.kind == CostKind::Reserved)
        .ok_or(ReleaseError::NoReservation {
            run_uid: request.run_uid,
        })?;
    let record = settle(reserved, request);
    let slot = release_slot(ports.refs, request.slot, request.lease_commit).map_err(|source| {
        ReleaseError::Slot {
            slot: request.slot,
            source,
        }
    })?;
    let cost = append_cost(ports.ledger, &record)
        .map_err(|source| ReleaseError::Final { slot, source })?;
    Ok(Settled { slot, record, cost })
}

/// The `final` record settling `reserved` as `request`'s review ended.
fn settle(reserved: CostRecord, request: &ReleaseRequest<'_>) -> CostRecord {
    let (aeu, roles) = match request.review {
        // The review reports its total only, not each stage's (the real review is M2).
        ReviewCost::Reported(aeu) => (aeu, Vec::new()),
        // The run cap: what the reservation holds, stage by stage.
        ReviewCost::RunCap | ReviewCost::Cancelled => (reserved.aeu, reserved.roles),
    };
    CostRecord {
        roles,
        aeu,
        kind: CostKind::Final,
        cancelled: request.review == ReviewCost::Cancelled,
        at: request.now,
        ..reserved
    }
}
