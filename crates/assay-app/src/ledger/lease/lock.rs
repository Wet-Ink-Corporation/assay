//! The per-head publish lock, `assay.ledger-lock/1` (02 §5, 06 §1, R3-09): every writer of the
//! verdict check holds `refs/heads/lock-<repo_id>-<head_sha>` across read, decide and write.

use std::borrow::Cow;
use std::time::Duration;

use assay_domain::RunUid;
use serde::{Deserialize, Serialize};

use super::document::{line_of, to_message};
use super::error::LeaseError;
use super::name::LeaseRef;
use super::refs::LedgerRefs;
use super::take::{Attempt, Expiring, Released, take};
use crate::config::CommitId;
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
    let read = |message: &str| read_lock(message, &lease);
    let attempt = Attempt {
        name: &name,
        message: &message,
        now,
    };
    let taken = take(refs, attempt, read)?;
    Ok(match taken.held {
        Some(lock_commit) => LockAcquired::Held(LockHold { lock_commit, lease }),
        None => LockAcquired::Busy,
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

impl Expiring for LockLease {
    fn expires_at(&self) -> LedgerTime {
        self.expires_at
    }
}

/// The ref of the lock `lease` is for.
fn lock_ref(lease: &LockLease) -> LeaseRef {
    LeaseRef::publish_lock(lease.run_uid.repo_id, &lease.head_sha)
}

/// The commit message of `lease`, refused when its own reader would refuse it.
fn write_lock(lease: &LockLease) -> Result<String, Corruption> {
    let message = to_message(&LockLine::from(lease))?;
    read_lock(&message, lease)?;
    Ok(message)
}

/// The lock lease a commit message carries, exactly as assay writes it, refused unless it is
/// for the same lock as `ours` (repository id and head).
fn read_lock(message: &str, ours: &LockLease) -> Result<LockLease, Corruption> {
    let line = line_of(message)?;
    let lease = LockLease::try_from(parse::<LockLine<'_>>(line)?)?;
    check_canonical(line, &LockLine::from(&lease), Newline::Stripped)?;
    if lease.run_uid.repo_id != ours.run_uid.repo_id || lease.head_sha != ours.head_sha {
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
