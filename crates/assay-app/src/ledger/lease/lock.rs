//! The per-head publish lock, `assay.ledger-lock/1` (02 §5, 06 §1, R3-09): every writer of the
//! verdict check holds `refs/heads/lock-<repo_id>-<head_sha>` across read, decide and write.

use std::borrow::Cow;
use std::num::NonZeroU64;
use std::time::Duration;

use assay_domain::RunUid;
use serde::{Deserialize, Serialize};

use super::document::{line_of, to_message};
use super::error::LeaseError;
use super::name::LeaseRef;
use super::refs::{LedgerRefs, RefTip};
use super::take::{Attempt, Expiring, Held, LeaseState, Released, take};
use crate::config::CommitId;
use crate::forge::Deadline;
use crate::ledger::lines::{Newline, check_canonical, check_schema, parse};
use crate::ledger::record::{field, is_repository};
use crate::ledger::{Corruption, LedgerTime};

/// The contract id every lock lease carries.
const LOCK_SCHEMA: &str = "assay.ledger-lock/1";

/// How long a publish lock lives (02 §5): a writer's read, decide and write take seconds.
pub const LOCK_TTL: Duration = Duration::from_mins(2);

/// What a writer asks the lock of a head for. The lock is the run's repository's:
/// `run_uid.repo_id` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRequest {
    /// The run writing the verdict check.
    pub run_uid: RunUid,
    /// Its repository, `owner/name`.
    pub repo: String,
    /// The head whose check it writes.
    pub head: CommitId,
}

/// A lock's lease: `{run_uid, repo, head_sha, expires_at}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockLease {
    /// The run holding the lock; its `repo_id` names the lock with `head_sha`.
    pub run_uid: RunUid,
    /// Its repository, `owner/name`.
    pub repo: String,
    /// The head locked.
    pub head_sha: CommitId,
    /// When the lock may be reaped: after this instant.
    pub expires_at: LedgerTime,
}

/// A lock held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHold {
    /// The commit the lock's ref points at, which releasing compares against.
    pub lock_commit: CommitId,
    /// The lease written.
    pub lease: LockLease,
}

impl LockHold {
    /// The deadline of every forge call made while holding the lock: the lease's `expires_at`
    /// (#290).
    #[must_use]
    pub const fn deadline(&self) -> Deadline {
        Deadline::at(self.lease.expires_at)
    }
}

/// Whether the lock was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockAcquired {
    /// This run holds the lock.
    Held(LockHold),
    /// Another writer holds it under a live lease; the caller waits or yields.
    Busy,
}

/// Takes the publish lock of `request.head` at `now`, for [`LOCK_TTL`]: creates its ref, and when
/// it is held by an expired lease, reaps that by compare-and-set and creates it once more.
///
/// The lock is held only through the ref this call created. A live lease is
/// [`LockAcquired::Busy`] even when it names `request.run_uid`: the lock's holder is the
/// workflow run, shared by all its jobs, so re-entering it would let two jobs of one run hold it
/// at once and the first release end the other's critical section (#288 R2-1). A creation
/// whose answer was lost within this call is settled by the refs adapter, which reads the ref
/// back; one lost in an earlier call is left to expire with its TTL.
///
/// # Errors
///
/// [`LeaseError::Invalid`] when the lease would break its contract, [`LeaseError::Corrupt`] when
/// the lock's lease cannot be read or names another head (it is not reaped), and
/// [`LeaseError::Refs`] when the refs cannot be read or written.
pub fn acquire_lock<R: LedgerRefs>(
    refs: &R,
    request: &LockRequest,
    now: LedgerTime,
) -> Result<LockAcquired, LeaseError<R::Error>> {
    let expires_at = now.checked_add(LOCK_TTL).ok_or(LeaseError::Invalid {
        problem: Corruption::Field("expires_at"),
    })?;
    let lease = LockLease {
        run_uid: request.run_uid,
        repo: request.repo.clone(),
        head_sha: request.head.clone(),
        expires_at,
    };
    let name = lock_ref(&lease);
    let message = write_lock(&lease).map_err(|problem| LeaseError::Invalid { problem })?;
    let read = |message: &str| read_lock(message, LockKey::of(&lease));
    let attempt = Attempt {
        name: &name,
        message: &message,
        now,
        holder: request.run_uid,
    };
    let taken = take(refs, attempt, read)?;
    Ok(match taken.held {
        Some(Held::Created(lock_commit)) => LockAcquired::Held(LockHold { lock_commit, lease }),
        // The holder is the workflow run, which all its jobs share: a live lease of this run
        // may be another job's critical section, so it is busy, never re-entered (#288 R2-1).
        Some(Held::Already { .. }) | None => LockAcquired::Busy,
    })
}

/// Releases the lock `hold` holds, by compare-and-set: a lock another writer took after this
/// one's expired is never released.
///
/// # Errors
///
/// [`LeaseError::Refs`] when the deletion could not be made or refused; the lock then expires
/// with its TTL.
pub fn release_lock<R: LedgerRefs>(
    refs: &R,
    hold: &LockHold,
) -> Result<Released, LeaseError<R::Error>> {
    refs.delete_if(&lock_ref(&hold.lease), &hold.lock_commit)
        .map(Released::from)
        .map_err(LeaseError::Refs)
}

/// Reaps the publish lock of `head` in repository `repo_id` when its lease expired before `now`,
/// by compare-and-set on the commit read: a lock taken again after the read is never deleted
/// (the ledger-side reconcile's backstop, 06 §4, F21). Returns the lease this call deleted.
///
/// # Errors
///
/// [`LeaseError::Corrupt`] when the lock's lease cannot be read or names another lock (it is not
/// reaped), and [`LeaseError::Refs`] when the ref cannot be read or deleted.
pub(crate) fn reap_expired_lock<R: LedgerRefs>(
    refs: &R,
    repo_id: NonZeroU64,
    head: &CommitId,
    now: LedgerTime,
) -> Result<Option<LockLease>, LeaseError<R::Error>> {
    let name = LeaseRef::publish_lock(repo_id, head);
    let Some(RefTip { commit, message }) = refs.read(&name).map_err(LeaseError::Refs)? else {
        return Ok(None);
    };
    let lease =
        read_lock(&message, LockKey { repo_id, head }).map_err(|problem| LeaseError::Corrupt {
            name: name.clone(),
            problem,
        })?;
    if LeaseState::at(lease.expires_at, now) == LeaseState::Live {
        return Ok(None);
    }
    Ok(
        match Released::from(refs.delete_if(&name, &commit).map_err(LeaseError::Refs)?) {
            Released::Released => Some(lease),
            Released::NotHeld => None,
        },
    )
}

impl Expiring for LockLease {
    fn expires_at(&self) -> LedgerTime {
        self.expires_at
    }

    fn holder(&self) -> RunUid {
        self.run_uid
    }
}

/// The ref of the lock `lease` is for.
fn lock_ref(lease: &LockLease) -> LeaseRef {
    LeaseRef::publish_lock(lease.run_uid.repo_id, &lease.head_sha)
}

/// The commit message of `lease`, refused when its own reader would refuse it.
fn write_lock(lease: &LockLease) -> Result<String, Corruption> {
    let message = to_message(&LockLine::from(lease))?;
    read_lock(&message, LockKey::of(lease))?;
    Ok(message)
}

/// Which lock a lease must be for: its ref's repository id and head.
#[derive(Clone, Copy)]
struct LockKey<'a> {
    repo_id: NonZeroU64,
    head: &'a CommitId,
}

impl<'a> LockKey<'a> {
    const fn of(lease: &'a LockLease) -> Self {
        Self {
            repo_id: lease.run_uid.repo_id,
            head: &lease.head_sha,
        }
    }
}

/// The lock lease a commit message carries, exactly as assay writes it, refused unless it is
/// for the lock `key` names (repository id and head).
fn read_lock(message: &str, key: LockKey<'_>) -> Result<LockLease, Corruption> {
    let line = line_of(message)?;
    let lease = LockLease::try_from(parse::<LockLine<'_>>(line)?)?;
    check_canonical(line, &LockLine::from(&lease), Newline::Stripped)?;
    if lease.run_uid.repo_id != key.repo_id || lease.head_sha != *key.head {
        return Err(Corruption::WrongRef);
    }
    Ok(lease)
}

/// The wire form of a lock lease (OUT-01).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LockLine<'a> {
    schema: Cow<'static, str>,
    run_uid: RunUid,
    repo: Cow<'a, str>,
    head_sha: String,
    expires_at: String,
}

impl<'a> From<&'a LockLease> for LockLine<'a> {
    fn from(lease: &'a LockLease) -> Self {
        Self {
            schema: Cow::Borrowed(LOCK_SCHEMA),
            run_uid: lease.run_uid,
            repo: Cow::Borrowed(&lease.repo),
            head_sha: lease.head_sha.to_string(),
            expires_at: lease.expires_at.to_string(),
        }
    }
}

impl TryFrom<LockLine<'_>> for LockLease {
    type Error = Corruption;

    fn try_from(line: LockLine<'_>) -> Result<Self, Corruption> {
        check_schema(&line.schema, LOCK_SCHEMA)?;
        if !is_repository(&line.repo) {
            return Err(Corruption::Field("repo"));
        }
        Ok(Self {
            run_uid: line.run_uid,
            repo: line.repo.into_owned(),
            head_sha: field(&line.head_sha, "head_sha")?,
            expires_at: field(&line.expires_at, "expires_at")?,
        })
    }
}
