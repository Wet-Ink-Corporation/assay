//! What the per-repo reconcile reports, why it stops, and what it writes to a check it closes.

use std::num::NonZeroU64;

use assay_domain::{ReasonCode, RunUid};
use thiserror::Error;

use crate::config::CommitId;
use crate::ledger::{LeaseError, LedgerReadError};
use crate::publish::{CheckConclusion, CheckRunId, CheckState, CheckUpdate};

/// [`STUCK_AFTER`](super::STUCK_AFTER) in minutes, as the closed check's summary states it.
pub(super) const STUCK_MINUTES: u64 = 50;

/// What one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChecksReconciled {
    /// The checks closed.
    pub closed: Vec<ClosedCheck>,
    /// The checks due to be closed that this sweep left; the next one tries again.
    pub skipped: Vec<SkippedCheck>,
}

/// A check the sweep closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedCheck {
    /// The PR.
    pub pr: NonZeroU64,
    /// The head the check is on.
    pub head: CommitId,
    /// The check.
    pub check: CheckRunId,
    /// How it was closed.
    pub closure: Closure,
}

/// How a check was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Closure {
    /// Stuck in progress on the PR's head: `failure (publish-error)`.
    Stuck,
    /// On a head a later push superseded: `cancelled`.
    Superseded,
}

/// A check due to be closed that the sweep left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCheck {
    /// The PR.
    pub pr: NonZeroU64,
    /// The head the check is on.
    pub head: CommitId,
    /// The check.
    pub check: CheckRunId,
    /// Why it was left.
    pub reason: Skip,
}

/// Why a check due to be closed was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Skip {
    /// Another writer holds the head's publish lock.
    LockBusy,
    /// Re-read under the lock, the PR's head no longer makes the check what it was found to be.
    HeadMoved,
    /// The check's `external_id` changed since it was read: another writer moved it.
    Moved,
    /// The check carries no `external_id`, so no write can be conditioned on it.
    NoStamp,
}

/// Which listing passed its cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Listing {
    /// The open PRs ([`MAX_OPEN_PULLS`](super::MAX_OPEN_PULLS)).
    OpenPulls,
    /// The App's checks on a head ([`MAX_HEAD_CHECKS`](super::MAX_HEAD_CHECKS)).
    Checks,
    /// The live runs on a head ([`MAX_HEAD_RUNS`](super::MAX_HEAD_RUNS)).
    Runs,
}

/// Why the per-repo reconcile stopped. Checks closed before the error stay closed.
#[derive(Debug, Error)]
pub enum ChecksReconcileError<F, L, R> {
    /// A re-run of the reconcile (R3-05): nothing was read or written.
    #[error("run attempt {attempt} is a re-run; only attempt 1 reconciles (R3-05)")]
    NotFirstAttempt {
        /// The attempt found.
        attempt: NonZeroU64,
    },
    /// GitHub could not be read or written.
    #[error("GitHub could not be read or written")]
    Forge(#[source] F),
    /// The ledger's push records or PR state could not be read.
    #[error("the ledger could not be read")]
    Ledger(#[source] LedgerReadError<L>),
    /// A publish lock could not be taken or released.
    #[error("a publish lock could not be taken or released")]
    Lock(#[source] LeaseError<R>),
    /// A listing passed its cap; it was not used.
    #[error("the {what:?} listing passed its cap of {limit}")]
    TooMany {
        /// The listing.
        what: Listing,
        /// Its cap.
        limit: usize,
    },
}

/// What closing a check as `closure` writes; its `external_id` names the reconcile run, and its
/// text is empty, so `assay status` reads a stuck check as refused with `publish-error` (#218).
pub(super) fn closure_update(closure: Closure, run_uid: RunUid) -> CheckUpdate {
    let external_id = format!("reconcile:{}", run_uid.run_id);
    match closure {
        Closure::Stuck => CheckUpdate {
            state: CheckState::Completed(CheckConclusion::Failure),
            title: format!("assay: failure ({})", ReasonCode::PublishError.code()),
            summary: format!(
                "assay's reconcile closed this check: it stayed in progress for more than \
                 {STUCK_MINUTES} minutes with no assay workflow run live on this head, so no run \
                 would publish it (06 §4). Push a commit or comment `/assay recheck` to run assay \
                 again."
            ),
            text: String::new(),
            external_id,
        },
        Closure::Superseded => CheckUpdate {
            state: CheckState::Completed(CheckConclusion::Cancelled),
            title: "assay: cancelled (superseded)".to_owned(),
            summary: "assay's reconcile closed this check: a later push superseded this head, \
                      so its run publishes nothing here (02 §5, 06 §4)."
                .to_owned(),
            text: String::new(),
            external_id,
        },
    }
}
