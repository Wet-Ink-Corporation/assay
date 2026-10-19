//! The review slots and their leases, `assay.ledger-lease/1` (06 §1, A-03, R3-06, R3-16): two
//! model-running review jobs across every repository, each holding `refs/heads/slot-N` from
//! `admit` until the `release` job.

use std::borrow::Cow;
use std::num::NonZeroU64;
use std::time::Duration;

use assay_domain::{Aeu, RunUid};
use serde::{Deserialize, Serialize};

use super::document::{line_of, to_message};
use super::error::LeaseError;
use super::name::{LeaseRef, Slot};
use super::refs::{LedgerRefs, RefTip};
use super::take::{Attempt, Expiring, LeaseState, Released, take};
use crate::config::CommitId;
use crate::ledger::aeu_text::parse_aeu;
use crate::ledger::lines::{Newline, check_canonical, check_schema, parse};
use crate::ledger::record::{field, is_repository};
use crate::ledger::{Corruption, LeaseReservation, LedgerTime};

/// The contract id every slot lease carries.
const LEASE_SCHEMA: &str = "assay.ledger-lease/1";

/// How long a slot lease lives (06 §1): longer than [`MAX_REVIEW_TIMEOUT`], so a live review's
/// lease never expires.
pub const SLOT_TTL: Duration = Duration::from_mins(45);

/// The longest `timeout-minutes` the `review` job may carry (06 §1, R3-06): below
/// [`SLOT_TTL`], so a review is cancelled before its lease could be reaped.
pub const MAX_REVIEW_TIMEOUT: Duration = Duration::from_mins(40);

/// What `admit` asks a slot for: the run and what its admission reserves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRequest {
    /// The run that will hold the slot.
    pub run_uid: RunUid,
    /// The consumer repository, `owner/name`.
    pub repo: String,
    /// The PR reviewed.
    pub pr: NonZeroU64,
    /// What the admission reserves, counted by the budget read until the run's cost record
    /// settles it.
    pub reserved_aeu: Aeu,
}

/// A slot's lease, the message of the commit its ref points at: `{run_uid, repo, pr,
/// reserved_aeu, expires_at}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLease {
    /// The run holding the slot.
    pub run_uid: RunUid,
    /// The consumer repository, `owner/name`.
    pub repo: String,
    /// The PR reviewed.
    pub pr: NonZeroU64,
    /// What the admission reserved.
    pub reserved_aeu: Aeu,
    /// When the lease may be reaped: after this instant.
    pub expires_at: LedgerTime,
}

/// A slot held: what the `release` job needs, as `admit`'s `slot` and `lease_commit` outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotHold {
    /// The slot.
    pub slot: Slot,
    /// The commit its ref points at, which releasing compares against.
    pub lease_commit: CommitId,
    /// The lease written.
    pub lease: SlotLease,
}

/// Whether a slot was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotAcquired {
    /// This run holds a slot.
    Held(SlotHold),
    /// Every slot is held by a live lease: `admit` waits, then defers with `slot-busy`.
    Busy,
}

/// What an acquisition did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotAcquisition {
    /// The slot taken, or none.
    pub acquired: SlotAcquired,
    /// The expired leases this acquisition reaped, whose runs the reconcile back-fills.
    pub reaped: Vec<SlotView>,
}

/// A slot's ref as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotView {
    /// The slot.
    pub slot: Slot,
    /// The commit its ref points at.
    pub lease_commit: CommitId,
    /// The lease that commit carries.
    pub lease: SlotLease,
    /// Whether it still holds the slot.
    pub state: LeaseState,
}

impl SlotView {
    /// What the lease reserves, for the budget read: a lease counts while it is live and after
    /// it expired until it is reaped (06 §1).
    #[must_use]
    pub const fn reservation(&self) -> LeaseReservation {
        LeaseReservation {
            run_uid: self.lease.run_uid,
            reserved_aeu: self.lease.reserved_aeu,
        }
    }
}

impl Expiring for SlotLease {
    fn expires_at(&self) -> LedgerTime {
        self.expires_at
    }
}

/// Takes a slot for `request` at `now`, trying `slot-0` then `slot-1`.
///
/// It creates the slot's ref at a commit carrying the lease, which expires [`SLOT_TTL`] after
/// `now`. A slot held by an expired lease is reaped by compare-and-set first (06 §1: `admit`
/// reaps before it waits).
///
/// # Errors
///
/// [`LeaseError::Invalid`] when the lease would break its contract (nothing is written),
/// [`LeaseError::Corrupt`] when a slot's lease cannot be read (none is reaped or taken after it),
/// and [`LeaseError::Refs`] when the refs cannot be read or written.
pub fn acquire_slot<R: LedgerRefs>(
    refs: &R,
    request: &SlotRequest,
    now: LedgerTime,
) -> Result<SlotAcquisition, LeaseError<R::Error>> {
    let expires_at = now.checked_add(SLOT_TTL).ok_or(LeaseError::Invalid {
        problem: Corruption::Field("expires_at"),
    })?;
    let lease = SlotLease {
        run_uid: request.run_uid,
        repo: request.repo.clone(),
        pr: request.pr,
        reserved_aeu: request.reserved_aeu,
        expires_at,
    };
    let message = write_lease(&lease).map_err(|problem| LeaseError::Invalid { problem })?;
    let mut reaped = Vec::new();
    for slot in Slot::ALL {
        let name = LeaseRef::slot(slot);
        let attempt = Attempt {
            name: &name,
            message: &message,
            now,
        };
        let taken = take(refs, attempt, read_lease)?;
        if let Some((lease_commit, lease)) = taken.reaped {
            reaped.push(SlotView {
                slot,
                lease_commit,
                lease,
                state: LeaseState::Expired,
            });
        }
        if let Some(lease_commit) = taken.held {
            let hold = SlotHold {
                slot,
                lease_commit,
                lease,
            };
            return Ok(SlotAcquisition {
                acquired: SlotAcquired::Held(hold),
                reaped,
            });
        }
    }
    Ok(SlotAcquisition {
        acquired: SlotAcquired::Busy,
        reaped,
    })
}

/// Releases `slot` by compare-and-set: deletes its ref only while it points at `lease_commit`,
/// so a holder whose lease expired and was reaped never deletes a newer holder's (A-33).
///
/// # Errors
///
/// [`LeaseError::Refs`] when the deletion could not be made or refused; the lease then expires
/// with its TTL and is reaped.
pub fn release_slot<R: LedgerRefs>(
    refs: &R,
    slot: Slot,
    lease_commit: &CommitId,
) -> Result<Released, LeaseError<R::Error>> {
    refs.delete_if(&LeaseRef::slot(slot), lease_commit)
        .map(Released::from)
        .map_err(LeaseError::Refs)
}

/// Every held slot at `now`, in slot order, with its lease and whether it expired: the leases the
/// budget read counts and the reconcile reaps.
///
/// # Errors
///
/// [`LeaseError::Corrupt`] when a slot's lease cannot be read, and [`LeaseError::Refs`] when the
/// refs cannot be read: the budget is then unknown and `admit` fails (06 §1).
pub fn read_slots<R: LedgerRefs>(
    refs: &R,
    now: LedgerTime,
) -> Result<Vec<SlotView>, LeaseError<R::Error>> {
    let mut views = Vec::with_capacity(Slot::ALL.len());
    for slot in Slot::ALL {
        let name = LeaseRef::slot(slot);
        let Some(RefTip { commit, message }) = refs.read(&name).map_err(LeaseError::Refs)? else {
            continue;
        };
        let lease =
            read_lease(&message).map_err(|problem| LeaseError::Corrupt { name, problem })?;
        views.push(SlotView {
            slot,
            lease_commit: commit,
            state: LeaseState::at(lease.expires_at, now),
            lease,
        });
    }
    Ok(views)
}

/// Reaps every slot whose lease expired before `now`, each by compare-and-set on the commit read,
/// and returns those this call deleted (the reconcile's backstop, 06 §1, F17).
///
/// # Errors
///
/// As [`read_slots`], and [`LeaseError::Refs`] when a deletion fails.
pub fn reap_expired_slots<R: LedgerRefs>(
    refs: &R,
    now: LedgerTime,
) -> Result<Vec<SlotView>, LeaseError<R::Error>> {
    let mut reaped = Vec::new();
    for view in read_slots(refs, now)? {
        if view.state == LeaseState::Live {
            continue;
        }
        if release_slot(refs, view.slot, &view.lease_commit)? == Released::Released {
            reaped.push(view);
        }
    }
    Ok(reaped)
}

/// The commit message of `lease`, refused when its own reader would refuse it.
fn write_lease(lease: &SlotLease) -> Result<String, Corruption> {
    let message = to_message(&LeaseLine::from(lease))?;
    read_lease(&message)?;
    Ok(message)
}

/// The slot lease a commit message carries, exactly as assay writes it.
fn read_lease(message: &str) -> Result<SlotLease, Corruption> {
    let line = line_of(message)?;
    let lease = SlotLease::try_from(parse::<LeaseLine<'_>>(line)?)?;
    check_canonical(line, &LeaseLine::from(&lease), Newline::Stripped)?;
    Ok(lease)
}

/// The wire form of a slot lease (OUT-01).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseLine<'a> {
    schema: Cow<'static, str>,
    run_uid: RunUid,
    repo: Cow<'a, str>,
    pr: NonZeroU64,
    reserved_aeu: String,
    expires_at: String,
}

impl<'a> From<&'a SlotLease> for LeaseLine<'a> {
    fn from(lease: &'a SlotLease) -> Self {
        Self {
            schema: Cow::Borrowed(LEASE_SCHEMA),
            run_uid: lease.run_uid,
            repo: Cow::Borrowed(&lease.repo),
            pr: lease.pr,
            reserved_aeu: lease.reserved_aeu.to_string(),
            expires_at: lease.expires_at.to_string(),
        }
    }
}

impl TryFrom<LeaseLine<'_>> for SlotLease {
    type Error = Corruption;

    fn try_from(line: LeaseLine<'_>) -> Result<Self, Corruption> {
        check_schema(&line.schema, LEASE_SCHEMA)?;
        if !is_repository(&line.repo) {
            return Err(Corruption::Field("repo"));
        }
        Ok(Self {
            run_uid: line.run_uid,
            repo: line.repo.into_owned(),
            pr: line.pr,
            reserved_aeu: parse_aeu(&line.reserved_aeu).ok_or(Corruption::Field("reserved_aeu"))?,
            expires_at: field(&line.expires_at, "expires_at")?,
        })
    }
}
