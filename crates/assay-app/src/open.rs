//! The `open` use case (S0, 04 §1, 06 §4.1): the single App verdict check for (head, name),
//! found or created and stamped `in_progress` with `pending:<run_id>:<seq>` under the per-head
//! publish lock, after the opener record of an `opened` or the push record of a `synchronize`
//! is in the ledger (02 §5, R3-07, R3-09, R3-21).
//!
//! `open` holds no `contents` permission and reads no PR content (05 §4): it calls only
//! [`Forge::find_or_create_check`] and [`Forge::update_check_if`], the push records of the
//! [`Ledger`] and the publish lock of the [`LedgerRefs`].

mod check;
mod stamp;

use std::num::NonZeroU64;

pub use stamp::PendingStamp;

use crate::config::CommitId;
use crate::forge::Forge;
use crate::ledger::{
    LeaseError, Ledger, LedgerReadError, LedgerRefs, LedgerTime, LedgerWriteError, LockAcquired,
    LockHold, LockRequest, PushAppended, PushDraft, PushKind, Released, acquire_lock, append_push,
    read_pushes, release_lock,
};
use crate::publish::{CheckName, CheckRunId, Unlock};
use crate::status::UserId;
use assay_domain::RunUid;

/// How many times `open` tries the publish lock again after finding it busy, pausing before
/// each (S0: retry ×3, then abort visibly).
pub const MAX_LOCK_RETRIES: u8 = 3;

/// Time as `open` needs it: the instant the lock's lease runs from, and a pause between lock
/// attempts. The composition root reads the system clock; tests script it (TST-10).
pub trait OpenClock {
    /// Why the time could not be read.
    type Error: std::error::Error;

    /// The current instant.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the clock cannot be read or is outside what the ledger records.
    fn now(&self) -> Result<LedgerTime, Self::Error>;

    /// Waits before lock attempt `retry` (1 to [`MAX_LOCK_RETRIES`]); the implementation picks
    /// the delay, bounded well under the lock's 2-minute TTL.
    fn pause(&self, retry: u8);
}

/// The event behind the workflow run, as far as `open` acts on it.
///
/// Only `opened` (an opener record) and `synchronize` (a push record) write to the ledger
/// (R3-07); the composition root refuses any other event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenTrigger {
    /// `pull_request_target` `opened`: the head is the one the PR arrived with.
    Opened {
        /// The event's sender: the opener, never a pusher.
        sender: UserId,
    },
    /// `pull_request_target` `synchronize`: a push to the PR's head branch.
    Synchronize(PushEvent),
    /// `pull_request_target` `reopened`.
    Reopened,
    /// `pull_request_target` `ready_for_review`.
    ReadyForReview,
    /// `pull_request_target` `edited` (a base change).
    Edited,
    /// `workflow_dispatch`: a requeue or a retry (06 §1).
    Dispatch,
}

/// A `synchronize` event's push; the head after it is the request's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushEvent {
    /// The head before the push (`before`).
    pub before: CommitId,
    /// The event's sender, who pushed.
    pub sender: UserId,
    /// Whether the push rewrote history, as the caller determined it.
    pub forced: bool,
}

/// What one run of `open` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRequest<'a> {
    /// The workflow run; only its first attempt may run (R3-05).
    pub run_uid: RunUid,
    /// The repository, `owner/name`, named in the lock's lease.
    pub repo: &'a str,
    /// The PR number.
    pub pr: NonZeroU64,
    /// The PR's head: the check's commit and a push's `after`.
    pub head: &'a CommitId,
    /// The PR's base branch.
    pub base_ref: &'a str,
    /// The repository's default branch, which names the check `assay / verdict` (R3-03).
    pub default_branch: &'a str,
    /// The event behind the run.
    pub trigger: &'a OpenTrigger,
    /// The workflow run's `created_at`: a push record's `at`, the push clock (04 §5).
    pub run_created_at: LedgerTime,
}

/// The ports `open` acts through.
#[derive(Debug, Clone, Copy)]
pub struct OpenPorts<'a, F, L, R, C> {
    /// GitHub, with the `assay-verdict` App's `checks: write` token.
    pub forge: &'a F,
    /// The ledger's files, with the `assay-ledger-writer` token.
    pub ledger: &'a L,
    /// The ledger's refs (the publish lock), with the same token.
    pub refs: &'a R,
    /// The time.
    pub clock: &'a C,
}

/// What [`open`] did.
#[derive(Debug)]
pub struct Opened<F, R> {
    /// The verdict check.
    pub check: CheckRunId,
    /// Its name.
    pub name: CheckName,
    /// This run's stamp.
    pub stamp: PendingStamp,
    /// What was written to the check.
    pub stamped: Stamped<F>,
    /// The opener record of an `opened` or the push record of a `synchronize`.
    pub push: Option<PushAppended>,
    /// What became of the publish lock.
    pub unlock: Unlock<LeaseError<R>>,
}

/// What [`open`] wrote to the verdict check, under the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stamped<F> {
    /// There was none; it was created `in_progress` with this run's stamp.
    Created,
    /// It was not completed and carried an older stamp or a decision key; it now carries this
    /// run's stamp, `in_progress`. Its `started_at` is unchanged.
    Restamped,
    /// It was already `in_progress` with this run's stamp; nothing was written.
    Unchanged,
    /// It was completed and is now `in_progress` with this run's stamp, which a read-back
    /// confirmed.
    Reopened,
    /// It was completed and could not be reopened, so it was published as a completed `failure
    /// (re-deciding)` with this run's stamp (A-30), and the run continues.
    ReDeciding(ReopenRefused<F>),
    /// It carries the stamp of a newer run; nothing was written and this run yields.
    Yielded {
        /// The stamp found.
        found: String,
    },
}

/// Why a completed check was not reopened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReopenRefused<F> {
    /// GitHub refused the write.
    Rejected(F),
    /// GitHub took the write but the check read back otherwise than `in_progress` with this
    /// run's stamp.
    KeptConclusion,
}

/// The forge call that failed under the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenStep {
    /// Finding or creating the check.
    FindOrCreate,
    /// Writing this run's stamp.
    Restamp,
    /// Reading a reopened check back.
    Reread,
    /// Publishing `failure (re-deciding)`.
    ReDeciding,
}

/// Why [`open`] aborted. Each one fails the run visibly (S0, 06 §1).
#[derive(Debug, thiserror::Error)]
pub enum OpenError<F, L, R, C> {
    /// The run is a re-run; recovery is a fresh run (R3-05). Nothing was called.
    #[error("open refuses run attempt {attempt}: only a first attempt runs")]
    NotFirstAttempt {
        /// The attempt.
        attempt: NonZeroU64,
    },
    /// The opener or push record could not be appended; no lock or check was touched.
    #[error("the push record could not be appended to the ledger")]
    Push(#[source] LedgerWriteError<L>),
    /// The PR's push records could not be read for the stamp's number; no lock or check was
    /// touched.
    #[error("the push records could not be read from the ledger")]
    Pushes(#[source] LedgerReadError<L>),
    /// The clock could not be read.
    #[error("the clock could not be read")]
    Clock {
        /// Why.
        #[source]
        source: C,
        /// What became of the lock.
        unlock: Unlock<LeaseError<R>>,
    },
    /// The publish lock could not be taken; no check was touched.
    #[error("the publish lock could not be taken")]
    Lock(#[source] LeaseError<R>),
    /// Another writer held the publish lock through every attempt; no check was touched.
    #[error("the publish lock was busy for {attempts} attempts")]
    LockBusy {
        /// How many attempts were made.
        attempts: u8,
    },
    /// The lock's lease ran out before a check write, which was not made.
    #[error("the publish lock's lease ran out before the check was written")]
    LockExpired {
        /// What became of the lock.
        unlock: Unlock<LeaseError<R>>,
    },
    /// GitHub failed (after the adapter's own retries).
    #[error("GitHub could not be read or written ({step:?})")]
    Forge {
        /// The call.
        step: OpenStep,
        /// Why.
        #[source]
        source: F,
        /// What became of the lock.
        unlock: Unlock<LeaseError<R>>,
    },
    /// The check's stamp changed under the lock: a writer that does not take it, or a lock
    /// reaped from under this run. Nothing was written.
    #[error("the verdict check's stamp changed while the publish lock was held")]
    Moved {
        /// The stamp the check carries now.
        found: String,
        /// What became of the lock.
        unlock: Unlock<LeaseError<R>>,
    },
}

/// The [`OpenError`] of [`open`] over `ports`' error types.
pub type OpenFailure<F, L, R, C> = OpenError<
    <F as Forge>::Error,
    <L as Ledger>::Error,
    <R as LedgerRefs>::Error,
    <C as OpenClock>::Error,
>;

/// What [`open`] returns over `ports`' error types.
pub type OpenOutcome<F, L, R, C> =
    Result<Opened<<F as Forge>::Error, <R as LedgerRefs>::Error>, OpenFailure<F, L, R, C>>;

/// Opens the verdict check of `request.head` for this run (S0).
///
/// In order: a re-run is refused before any port call; an `opened` appends its opener record and
/// a `synchronize` its push record (any other event reads the latest record's number); the
/// publish lock is taken, retried
/// [`MAX_LOCK_RETRIES`] times when busy; under it the check is found or created and stamped
/// (see [`Stamped`]); the lock is released by compare-and-set. Forge calls are retried by the
/// adapter; `open` aborts on the error it returns.
///
/// The lock is re-entrant within one workflow run: a live lock of `request.run_uid` is taken as
/// held at its existing commit, with its own expiry (#288). So `open` and `decide`'s publish
/// must never hold it at the same time within one run; the workflow orders them.
///
/// # Errors
///
/// [`OpenError`]: every variant aborts the run visibly. A push record that cannot be appended
/// is [`OpenError::Push`] (06 §1).
pub fn open<F: Forge, L: Ledger, R: LedgerRefs, C: OpenClock>(
    request: &OpenRequest<'_>,
    ports: &OpenPorts<'_, F, L, R, C>,
) -> OpenOutcome<F, L, R, C> {
    let attempt = request.run_uid.run_attempt;
    if attempt.get() != 1 {
        return Err(OpenError::NotFirstAttempt { attempt });
    }
    let Recorded { push, seq } = record_push(request, ports.ledger)?;
    let stamp = PendingStamp::new(request.run_uid.run_id, seq);
    let name = CheckName::for_base(request.base_ref, request.default_branch);
    let hold = take_lock(request, ports)?;
    let target = check::Target {
        head: request.head,
        name: &name,
        stamp,
        hold: &hold,
    };
    let stamped = check::stamp(&target, ports);
    let unlock = match release_lock(ports.refs, &hold) {
        // `NotHeld`: the lease ran out after the last write (each write first checked it was
        // live) and another writer reaped it, so nothing of this run's is left to release.
        Ok(Released::Released | Released::NotHeld) => Unlock::Released,
        Err(error) => Unlock::Failed(error),
    };
    match stamped {
        Ok((check, stamped)) => Ok(Opened {
            check,
            name,
            stamp,
            stamped,
            push,
            unlock,
        }),
        Err(halt) => Err(halt.into_error(unlock)),
    }
}

/// The push record a run appended, if any, and the number its stamp carries.
struct Recorded {
    /// The record of an `opened` or a `synchronize`.
    push: Option<PushAppended>,
    /// The number of the PR's latest push record, 0 when there is none.
    seq: u64,
}

/// Appends the opener record of an `opened` or the push record of a `synchronize`; any other
/// event only reads the PR's push records for the latest number.
fn record_push<L: Ledger, F, R, C>(
    request: &OpenRequest<'_>,
    ledger: &L,
) -> Result<Recorded, OpenError<F, L::Error, R, C>> {
    // The draft stores its own commits; the request keeps its.
    let (kind, sender_id) = match request.trigger {
        OpenTrigger::Opened { sender } => (PushKind::Opener, *sender),
        OpenTrigger::Synchronize(event) => (
            PushKind::Push {
                before: event.before.clone(),
                forced: event.forced,
            },
            event.sender,
        ),
        OpenTrigger::Reopened
        | OpenTrigger::ReadyForReview
        | OpenTrigger::Edited
        | OpenTrigger::Dispatch => {
            let records = read_pushes(ledger, request.run_uid.repo_id, request.pr)
                .map_err(OpenError::Pushes)?;
            let seq = records.last().map_or(0, |record| record.seq.get());
            return Ok(Recorded { push: None, seq });
        }
    };
    let draft = PushDraft {
        run_uid: request.run_uid,
        pr: request.pr,
        kind,
        after: request.head.clone(),
        sender_id,
        at: request.run_created_at,
    };
    let appended = append_push(ledger, &draft).map_err(OpenError::Push)?;
    let seq = appended.record().seq.get();
    Ok(Recorded {
        push: Some(appended),
        seq,
    })
}

/// Takes the publish lock of `request.head`, pausing and trying again up to
/// [`MAX_LOCK_RETRIES`] times while another writer holds it.
fn take_lock<F: Forge, L: Ledger, R: LedgerRefs, C: OpenClock>(
    request: &OpenRequest<'_>,
    ports: &OpenPorts<'_, F, L, R, C>,
) -> Result<LockHold, OpenFailure<F, L, R, C>> {
    // The lease stores the repository and head.
    let lock = LockRequest {
        run_uid: request.run_uid,
        repo: request.repo.to_owned(),
        head: request.head.clone(),
    };
    for retry in 0..=MAX_LOCK_RETRIES {
        if retry > 0 {
            ports.clock.pause(retry);
        }
        let now = ports.clock.now().map_err(|source| OpenError::Clock {
            source,
            unlock: Unlock::NotTaken,
        })?;
        if let LockAcquired::Held(hold) =
            acquire_lock(ports.refs, &lock, now).map_err(OpenError::Lock)?
        {
            return Ok(hold);
        }
    }
    Err(OpenError::LockBusy {
        attempts: MAX_LOCK_RETRIES.saturating_add(1),
    })
}
