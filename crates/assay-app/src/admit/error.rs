//! Why `admit` failed. Every failure fails the `admit` job, so `decide` reads the LLM stage as not
//! run and the LLM-unavailable policy applies (04 §1 S1a, 06 §1, F16). No error quotes ledger,
//! bundle or comment contents.

use std::num::NonZeroU64;

use thiserror::Error;

use super::document::PlanDocument;
use super::events::EventFailure;
use crate::eligibility::CommandComments;
use crate::events::EventSink;
use crate::ledger::{LeaseError, Ledger, LedgerReadError, LedgerRefs, LedgerWriteError, SlotHold};
use crate::prompt_bundle::Role;
use crate::replay::BundleSource;
use crate::rereview::PriorCarryError;

/// Why `admit` failed. `L`, `R`, `B`, `C` and `S` are the ledger's, the refs', the bundle
/// store's, the comments' and the event sink's errors.
#[derive(Debug, Error)]
pub enum AdmitError<L, R, B, C, S> {
    /// The run is a re-run: privileged jobs run on attempt 1 only, and recovery is a fresh run
    /// (R3-05). Nothing was read.
    #[error("refused attempt {attempt}: admit runs on attempt 1 only; start a fresh run")]
    NotFirstAttempt {
        /// The attempt.
        attempt: NonZeroU64,
    },
    /// The plan's run description is refused; nothing was read.
    #[error(transparent)]
    Plan(PlanDocument),
    /// A ledger file could not be read: the budget, the evidence records or the command records
    /// are unknown. Nothing was written.
    #[error("the ledger could not be read")]
    LedgerRead(#[source] LedgerReadError<L>),
    /// The deferral record could not be written; no slot was taken.
    #[error("the deferral record could not be written")]
    LedgerWrite(#[source] LedgerWriteError<L>),
    /// The slots or a retry grant could not be read or taken; no slot is held.
    #[error("the ledger refs could not be read or taken")]
    Lease(#[source] LeaseError<R>),
    /// The carry is kept but the ledger has no evidence pointer to the previous run.
    #[error("the carry-over is kept but the ledger names no previous bundle")]
    NoPriorPointer,
    /// The previous run's findings could not be read from its bundle.
    #[error("the previous run's findings could not be read")]
    Prior(#[source] PriorCarryError<B>),
    /// A command record's comment could not be read to tell whether it still stands.
    #[error("a command record's comment could not be read")]
    Comments(#[source] C),
    /// The base suppress text is over [`MAX_SUPPRESS_BYTES`](super::MAX_SUPPRESS_BYTES).
    #[error("the base suppress text is {size} bytes, over its cap")]
    SuppressTooLarge {
        /// Its length in bytes.
        size: usize,
    },
    /// The context pack could not be written as JSON.
    #[error("the context pack could not be written")]
    Pack(#[source] serde_json::Error),
    /// An event could not be appended.
    #[error("an event could not be appended")]
    Events(#[source] S),
    /// A clock read before 1970 or after 9999.
    #[error("the clock reads outside 1970 to 9999")]
    Clock,
    /// A step after the slot was taken failed; boxed, as it is the largest failure by far.
    #[error(transparent)]
    AfterSlot(Box<AfterSlot<L, R, S>>),
}

/// A failure once a slot was taken.
///
/// The slot was released by compare-and-set unless `release` holds why it was not; then the
/// lease expires with its TTL and is reaped. A `reserved` record
/// already written stays in the ledger and counts against the budget until reconcile settles it
/// (06 §1); a retry grant already consumed (`consumed`) is gone, and only a fresh
/// `/assay retry` grants the role again.
#[derive(Debug, Error)]
#[error("admit failed holding slot {}", hold.slot)]
pub struct AfterSlot<L, R, S> {
    /// The slot that was held.
    pub hold: SlotHold,
    /// Why the slot could not be released, if it could not.
    pub release: Option<LeaseError<R>>,
    /// The role whose retry grant this run consumed before the failure, if any.
    pub consumed: Option<Role>,
    /// What failed.
    #[source]
    pub cause: Unwound<L, R, S>,
}

/// The [`Unwound`] of a run over the ports' error types.
pub(super) type UnwoundOf<L, R, S> =
    Unwound<<L as Ledger>::Error, <R as LedgerRefs>::Error, <S as EventSink>::Error>;

/// What failed after the slot was taken.
#[derive(Debug, Error)]
pub enum Unwound<L, R, S> {
    /// The cost records could not be read again once the slot was held.
    #[error("the ledger could not be read again once the slot was held")]
    Costs(#[source] LedgerReadError<L>),
    /// The slots could not be read again once the slot was held.
    #[error("the slots could not be read again once the slot was held")]
    Slots(#[source] LeaseError<R>),
    /// The budget read again once the slot was held deferred the run, and the slot could not be
    /// released (`release` says why); no deferral record was written.
    #[error("the budget deferred the run once the slot was held")]
    Deferred,
    /// The `reserved` record could not be written; it may or may not have landed.
    #[error("the reserved record could not be written")]
    Reserve(#[source] LedgerWriteError<L>),
    /// The retry grant could not be read or consumed.
    #[error("the retry grant could not be consumed")]
    Grant(#[source] LeaseError<R>),
    /// An event could not be appended.
    #[error("an event could not be appended")]
    Events(#[source] S),
    /// A clock read before 1970 or after 9999.
    #[error("the clock reads outside 1970 to 9999")]
    Clock,
    /// The run description could not be written.
    #[error(transparent)]
    Plan(PlanDocument),
}

/// The [`AdmitError`] of [`admit`](super::admit) over the ports' error types.
pub type AdmitFailure<L, R, B, C, S> = AdmitError<
    <L as Ledger>::Error,
    <R as LedgerRefs>::Error,
    <B as BundleSource>::Error,
    <C as CommandComments>::Error,
    <S as EventSink>::Error,
>;

impl<S> EventFailure<S> {
    /// The failure as `admit`'s, before a slot is held.
    pub(super) fn admit<L, R, B, C>(self) -> AdmitError<L, R, B, C, S> {
        match self {
            Self::Sink(error) => AdmitError::Events(error),
            Self::Clock => AdmitError::Clock,
        }
    }

    /// The failure as a step's after the slot was taken.
    pub(super) fn unwound<L, R>(self) -> Unwound<L, R, S> {
        match self {
            Self::Sink(error) => Unwound::Events(error),
            Self::Clock => Unwound::Clock,
        }
    }
}
