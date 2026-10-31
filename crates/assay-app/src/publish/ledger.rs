//! [`PublishLedger`] over the ledger's files and lease refs (D15): the per-head publish lock
//! through [`acquire_lock`] and [`release_lock`], and the evidence pointer and record through
//! [`update_state`] (02 §5, 06 §1, R3-08, R3-09, R3-17).

use std::num::NonZeroU64;

use assay_domain::RunUid;
use thiserror::Error;

use super::ports::{EvidenceEntry, PublishLedger};
use crate::config::CommitId;
use crate::ledger::{
    LeaseError, Ledger, LedgerRefs, LedgerWriteError, LockAcquired, LockHold, LockRequest,
    MAX_STATE_ENTRIES, PrState, Released, acquire_lock, release_lock, update_state,
};
use crate::open::{MAX_LOCK_RETRIES, OpenClock};

/// `decide`'s ledger port for one run: the ledger repository's files and refs, and the clock that
/// times the lock's lease and paces its retries.
///
/// The lock is taken only by creating its ref, so it is never re-entered, not even by another
/// job of this run (#288). A busy lock is tried again [`MAX_LOCK_RETRIES`] times, pausing
/// before each retry, and then [`PublishLedger::acquire`] gives `None`.
#[derive(Debug)]
pub struct LedgerPublish<'a, L, R, C> {
    /// The ledger's files.
    pub ledger: &'a L,
    /// The ledger's refs.
    pub refs: &'a R,
    /// The lease's instant and the pause between lock attempts.
    pub clock: &'a C,
    /// The run that publishes: it holds the lock and uploaded the bundle.
    pub run_uid: RunUid,
    /// Its repository, `owner/name`, as the lock lease records it.
    pub repo: &'a str,
}

/// Why [`LedgerPublish`] could not take or release the lock or record evidence.
#[derive(Debug, Error)]
pub enum LedgerPublishError<L, R, C> {
    /// The call named another repository than the run's; nothing was read or written.
    #[error("the call names repository {repo_id}, not the run's")]
    OtherRepository {
        /// The repository id the call named.
        repo_id: NonZeroU64,
    },
    /// The evidence pointer names another run or attempt than this one; nothing was written.
    #[error("the evidence pointer names another run than the publishing one")]
    OtherRun,
    /// The clock could not be read.
    #[error("could not read the clock")]
    Clock(#[source] C),
    /// The lock could not be taken, or its release could not be made.
    #[error("could not take or release the publish lock")]
    Lease(#[source] LeaseError<R>),
    /// The lock was no longer this run's to release: its lease expired and another writer
    /// reaped it, so writes made under it may have overlapped that writer's.
    #[error("the publish lock was no longer held when it was released")]
    NotHeld,
    /// The PR's state could not be written.
    #[error("could not record the evidence in the PR's state")]
    State(#[source] LedgerWriteError<L>),
}

// `Error::source` hands out `&(dyn Error + 'static)`, so a chained cause must be `'static`.
impl<L: Ledger, R: LedgerRefs, C: OpenClock> PublishLedger for LedgerPublish<'_, L, R, C>
where
    L::Error: 'static,
    R::Error: 'static,
    C::Error: 'static,
{
    type Error = LedgerPublishError<L::Error, R::Error, C::Error>;
    type Lease = LockHold;

    fn acquire(
        &self,
        repo_id: NonZeroU64,
        head: &CommitId,
    ) -> Result<Option<LockHold>, Self::Error> {
        self.own_repository(repo_id)?;
        // The lease stores the repository and head.
        let request = LockRequest {
            run_uid: self.run_uid,
            repo: self.repo.to_owned(),
            head: head.clone(),
        };
        for retry in 0..=MAX_LOCK_RETRIES {
            if retry > 0 {
                self.clock.pause(retry);
            }
            let now = self.clock.now().map_err(LedgerPublishError::Clock)?;
            if let LockAcquired::Held(hold) =
                acquire_lock(self.refs, &request, now).map_err(LedgerPublishError::Lease)?
            {
                return Ok(Some(hold));
            }
        }
        Ok(None)
    }

    fn release(&self, lease: LockHold) -> Result<(), Self::Error> {
        match release_lock(self.refs, &lease).map_err(LedgerPublishError::Lease)? {
            Released::Released => Ok(()),
            Released::NotHeld => Err(LedgerPublishError::NotHeld),
        }
    }

    fn record_evidence(&self, entry: &EvidenceEntry) -> Result<(), Self::Error> {
        self.own_repository(entry.repo_id)?;
        let pointer = &entry.pointer;
        if (pointer.run_id, pointer.run_attempt) != (self.run_uid.run_id, self.run_uid.run_attempt)
        {
            return Err(LedgerPublishError::OtherRun);
        }
        update_state(self.ledger, entry.repo_id, entry.pr, |state| {
            Some(with_evidence(state, entry))
        })
        .map(|_updated| ())
        .map_err(LedgerPublishError::State)
    }
}

impl<L, R, C> LedgerPublish<'_, L, R, C> {
    /// Refuses a repository id other than the run's.
    const fn own_repository<E, F, G>(
        &self,
        repo_id: NonZeroU64,
    ) -> Result<(), LedgerPublishError<E, F, G>> {
        if repo_id.get() == self.run_uid.repo_id.get() {
            Ok(())
        } else {
            Err(LedgerPublishError::OtherRepository { repo_id })
        }
    }
}

/// `state` with `entry` recorded: the pointer moved to it unless it names a later run (run ids
/// grow, so a later run's bundle stays the latest), and its record replacing the one of the same
/// evidence key, or appended, dropping the oldest record when the list is full (dropping one
/// only loses a reuse, never grants one).
fn with_evidence(state: &PrState, entry: &EvidenceEntry) -> PrState {
    // The update writes a new state from the one shown.
    let mut next = state.clone();
    let later = next
        .evidence
        .as_ref()
        .is_some_and(|current| current.run_id > entry.pointer.run_id);
    if !later {
        // The state stores the pointer.
        next.evidence = Some(entry.pointer.clone());
    }
    let records = &mut next.evidence_records;
    // The state stores the record.
    let record = entry.record.clone();
    if let Some(kept) = records
        .iter_mut()
        .find(|kept| kept.evidence_key == record.evidence_key)
    {
        *kept = record;
    } else {
        let excess = records
            .len()
            .saturating_sub(MAX_STATE_ENTRIES.saturating_sub(1));
        records.drain(..excess);
        records.push(record);
    }
    next
}
