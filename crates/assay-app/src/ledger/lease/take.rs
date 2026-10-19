//! Taking one lease ref: create it, and when it is held by an expired lease, reap that lease by
//! compare-and-set and create it once more. Bounded: at most two creations, one read and one
//! deletion per ref.

use super::error::LeaseError;
use super::name::LeaseRef;
use super::refs::{LedgerRefs, RefCreation, RefDeletion};
use crate::config::CommitId;
use crate::ledger::{Corruption, LedgerTime};

/// Whether a lease still holds its ref at an instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaseState {
    /// At or before its expiry: never reaped.
    Live,
    /// Strictly after its expiry: reapable by compare-and-set.
    Expired,
}

impl LeaseState {
    /// The state at `now` of a lease expiring at `expires_at`.
    pub(super) fn at(expires_at: LedgerTime, now: LedgerTime) -> Self {
        if now > expires_at {
            Self::Expired
        } else {
            Self::Live
        }
    }
}

/// What releasing a held ref did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Released {
    /// The ref still held this lease and is gone.
    Released,
    /// The ref no longer held this lease (it expired and was reaped, and may be held by a newer
    /// one): nothing was deleted.
    NotHeld,
}

impl From<RefDeletion> for Released {
    fn from(deletion: RefDeletion) -> Self {
        match deletion {
            RefDeletion::Deleted => Self::Released,
            RefDeletion::Moved => Self::NotHeld,
        }
    }
}

/// What one attempt at a ref did.
pub(super) struct Take<L> {
    /// The commit of our lease when we now hold the ref.
    pub(super) held: Option<CommitId>,
    /// The expired lease our compare-and-set deleted, with its commit.
    pub(super) reaped: Option<(CommitId, L)>,
}

/// A lease kind with an expiry.
pub(super) trait Expiring {
    /// After this instant the lease may be reaped.
    fn expires_at(&self) -> LedgerTime;
}

/// One attempt to take a ref: which, with what lease message, and when.
#[derive(Clone, Copy)]
pub(super) struct Attempt<'a> {
    /// The ref.
    pub(super) name: &'a LeaseRef,
    /// The lease, as the message of the commit created.
    pub(super) message: &'a str,
    /// The instant expiry is judged at.
    pub(super) now: LedgerTime,
}

/// Takes `attempt.name` with the lease `attempt.message`: creates it; when it exists, reads its
/// lease with `read` (which refuses one that breaks a rule or names another ref), and when that
/// lease expired before `attempt.now`, deletes the ref on condition that it is unchanged and
/// creates it again. A ref released between our creation and our read is created again too.
pub(super) fn take<R: LedgerRefs, L: Expiring>(
    refs: &R,
    attempt: Attempt<'_>,
    read: impl Fn(&str) -> Result<L, Corruption>,
) -> Result<Take<L>, LeaseError<R::Error>> {
    let Attempt { name, now, .. } = attempt;
    if let RefCreation::Created(commit) = create(refs, attempt)? {
        return Ok(Take {
            held: Some(commit),
            reaped: None,
        });
    }
    let Some(tip) = refs.read(name).map_err(LeaseError::Refs)? else {
        return retake(refs, attempt, None);
    };
    let lease = read(&tip.message).map_err(|problem| LeaseError::Corrupt {
        name: name.clone(),
        problem,
    })?;
    if LeaseState::at(lease.expires_at(), now) == LeaseState::Live {
        return Ok(Take {
            held: None,
            reaped: None,
        });
    }
    let reaped = match refs
        .delete_if(name, &tip.commit)
        .map_err(LeaseError::Refs)?
    {
        RefDeletion::Deleted => Some((tip.commit, lease)),
        RefDeletion::Moved => None,
    };
    retake(refs, attempt, reaped)
}

/// The second and last creation of the ref.
fn retake<R: LedgerRefs, L>(
    refs: &R,
    attempt: Attempt<'_>,
    reaped: Option<(CommitId, L)>,
) -> Result<Take<L>, LeaseError<R::Error>> {
    let held = match create(refs, attempt)? {
        RefCreation::Created(commit) => Some(commit),
        RefCreation::Exists => None,
    };
    Ok(Take { held, reaped })
}

fn create<R: LedgerRefs>(
    refs: &R,
    attempt: Attempt<'_>,
) -> Result<RefCreation, LeaseError<R::Error>> {
    refs.create(attempt.name, attempt.message)
        .map_err(LeaseError::Refs)
}
