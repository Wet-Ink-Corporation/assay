//! The single ledger-side reconcile (06 §4, §1, F17, F21, R3-16): run in the ledger repository
//! itself, it reaps expired slot leases and publish locks by compare-and-set and back-fills the
//! `final` cost record of every run whose `release` never wrote one.

use std::collections::HashSet;
use std::num::NonZeroU64;

use assay_domain::RunUid;
use thiserror::Error;

use super::locks::{LockListing, MAX_PUBLISH_LOCKS, lock_key};
use crate::config::CommitId;
use crate::ledger::{
    Appended, CostKind, CostRecord, LeaseError, Ledger, LedgerReadError, LedgerTime,
    LedgerWriteError, LockLease, SLOT_TTL, SlotView, append_cost, read_costs, read_slots,
    reap_expired_lock, reap_expired_slots,
};

/// What the ledger-side reconcile is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerReconcileRequest {
    /// The reconcile's own run; only its first attempt runs (R3-05).
    pub run_uid: RunUid,
    /// The instant expiry is judged at and back-fills are written at.
    pub now: LedgerTime,
}

/// What one sweep did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerReconciled {
    /// The expired slot leases this sweep deleted.
    pub reaped_slots: Vec<SlotView>,
    /// The expired publish locks this sweep deleted.
    pub reaped_locks: Vec<LockLease>,
    /// The `final` records written: charged at the run's reservation, `cancelled: true`.
    pub back_filled: Vec<CostRecord>,
    /// Runs whose `final` record another writer (a late `release`) appended during the sweep;
    /// nothing was written for them.
    pub already_settled: Vec<RunUid>,
    /// Leases this sweep reaped whose run has no cost record at all: no record can be written
    /// for them (a lease carries no `run_key`, head or tier), so they are reported instead.
    pub unrecorded: Vec<SlotView>,
}

/// Why the ledger-side reconcile stopped. Work done before the error stays done; the next sweep
/// takes up the rest.
#[derive(Debug, Error)]
pub enum LedgerReconcileError<L, R> {
    /// A re-run of the reconcile (R3-05): nothing was read or written.
    #[error("run attempt {attempt} is a re-run; only attempt 1 reconciles (R3-05)")]
    NotFirstAttempt {
        /// The attempt found.
        attempt: NonZeroU64,
    },
    /// The slots could not be read or reaped, or a slot's lease is unreadable (it is kept).
    #[error("the slots could not be reaped")]
    Slots(#[source] LeaseError<R>),
    /// The cost records could not be read.
    #[error("the cost records could not be read")]
    Costs(#[source] LedgerReadError<L>),
    /// A back-fill could not be written.
    #[error("the back-fill of run {} could not be written", run_uid.run_id)]
    BackFill {
        /// The run.
        run_uid: RunUid,
        /// Why.
        #[source]
        source: LedgerWriteError<L>,
    },
    /// The publish locks could not be listed.
    #[error("the publish locks could not be listed")]
    ListLocks(#[source] R),
    /// More publish locks than [`MAX_PUBLISH_LOCKS`] exist; nothing was reaped or written.
    #[error("more than {limit} publish locks exist")]
    TooManyLocks {
        /// The cap.
        limit: usize,
    },
    /// A ref under `refs/heads/lock-` is not named as assay names a lock; nothing was reaped or
    /// written. Its name is untrusted text, displayed escaped and cut at 120 characters.
    #[error("the ref {} is not a publish lock assay names", shown(.name))]
    LockName {
        /// The ref's full name, as listed.
        name: String,
    },
    /// A publish lock could not be read or reaped, or its lease is unreadable (it is kept).
    #[error("a publish lock could not be reaped")]
    Locks(#[source] LeaseError<R>),
}

/// How many characters of a refused lock name an error shows.
const SHOWN_NAME_CHARS: usize = 120;

/// `name` escaped (no control character reaches a terminal or log) and cut after
/// [`SHOWN_NAME_CHARS`] characters, marked with `…` when cut.
fn shown(name: &str) -> String {
    let mut chars = name.chars();
    let mut shown: String = chars
        .by_ref()
        .take(SHOWN_NAME_CHARS)
        .flat_map(char::escape_debug)
        .collect();
    if chars.next().is_some() {
        shown.push('…');
    }
    shown
}

/// Runs one ledger-side sweep at `request.now`.
///
/// It first lists the publish locks and refuses the sweep, before any reap or write, when they
/// pass [`MAX_PUBLISH_LOCKS`] or one is not named as assay names a lock; then it reaps every
/// expired slot lease, back-fills the unsettled reservations whose lease is gone, and reaps
/// every expired publish lock.
///
/// A run is back-filled when its `reserved` record has no `final` one, no slot's lease names it,
/// and the reservation is older than [`SLOT_TTL`] (a run's review never outlives its lease). The
/// record written is the reservation as `final`, `cancelled: true`, at `now`: the run cap the
/// admission reserved (06 §1), since nothing records what a run that never released spent.
/// [`append_cost`] writes nothing when the run's `final` record is already there, so a second
/// sweep, or a `release` landing during this one, never adds a second record (R3-16).
///
/// # Errors
///
/// [`LedgerReconcileError::NotFirstAttempt`] for a re-run, before any port is called; otherwise
/// the first port failure, unreadable lease or refused listing, after the work before it.
pub fn reconcile_ledger<L: Ledger, R: LockListing>(
    ledger: &L,
    refs: &R,
    request: &LedgerReconcileRequest,
) -> Result<LedgerReconciled, LedgerReconcileError<L::Error, R::Error>> {
    let attempt = request.run_uid.run_attempt;
    if attempt.get() != 1 {
        return Err(LedgerReconcileError::NotFirstAttempt { attempt });
    }
    let now = request.now;
    let locks = publish_locks(refs)?;
    let reaped_slots = reap_expired_slots(refs, now).map_err(LedgerReconcileError::Slots)?;
    let holding: HashSet<RunUid> = read_slots(refs, now)
        .map_err(LedgerReconcileError::Slots)?
        .iter()
        .map(|view| view.lease.run_uid)
        .collect();
    let window = read_costs(ledger, now).map_err(LedgerReconcileError::Costs)?;
    let records = window.records();
    let recorded: HashSet<RunUid> = records.iter().map(|record| record.run_uid).collect();
    let mut back_filled = Vec::new();
    let mut already_settled = Vec::new();
    for reserved in unsettled(records, &holding, now) {
        // A new record: the reservation's fields, settled. The window keeps the reservation.
        let record = CostRecord {
            kind: CostKind::Final,
            cancelled: true,
            at: now,
            ..reserved.clone()
        };
        match append_cost(ledger, &record).map_err(|source| LedgerReconcileError::BackFill {
            run_uid: record.run_uid,
            source,
        })? {
            Appended::Recorded => back_filled.push(record),
            Appended::AlreadyRecorded(_) => already_settled.push(record.run_uid),
        }
    }
    // Reported twice on purpose: reaped, and among the reaped, the ones no record covers.
    let unrecorded = reaped_slots
        .iter()
        .filter(|view| !recorded.contains(&view.lease.run_uid))
        .cloned()
        .collect();
    let reaped_locks = reap_locks(refs, locks, now)?;
    Ok(LedgerReconciled {
        reaped_slots,
        reaped_locks,
        back_filled,
        already_settled,
        unrecorded,
    })
}

/// The `reserved` records with no `final` record among `records`, whose run holds no slot and
/// whose reservation is older than [`SLOT_TTL`] at `now`.
fn unsettled<'r>(
    records: &'r [CostRecord],
    holding: &HashSet<RunUid>,
    now: LedgerTime,
) -> impl Iterator<Item = &'r CostRecord> {
    let settled: HashSet<RunUid> = records
        .iter()
        .filter(|record| record.kind == CostKind::Final)
        .map(|record| record.run_uid)
        .collect();
    records.iter().filter(move |record| {
        record.kind == CostKind::Reserved
            && !settled.contains(&record.run_uid)
            && !holding.contains(&record.run_uid)
            && record
                .at
                .checked_add(SLOT_TTL)
                .is_some_and(|lease_end| now > lease_end)
    })
}

/// Publish locks by repository id and head.
type LockKeys = Vec<(NonZeroU64, CommitId)>;

/// The publish locks by repository id and head, refused when they pass [`MAX_PUBLISH_LOCKS`] or
/// one is not named as assay names a lock.
fn publish_locks<L, R: LockListing>(
    refs: &R,
) -> Result<LockKeys, LedgerReconcileError<L, R::Error>> {
    let names = refs
        .publish_locks(MAX_PUBLISH_LOCKS.saturating_add(1))
        .map_err(LedgerReconcileError::ListLocks)?;
    if names.len() > MAX_PUBLISH_LOCKS {
        return Err(LedgerReconcileError::TooManyLocks {
            limit: MAX_PUBLISH_LOCKS,
        });
    }
    names
        .into_iter()
        .map(|name| lock_key(&name).ok_or(LedgerReconcileError::LockName { name }))
        .collect()
}

/// Reaps each of `locks` that expired, by compare-and-set on the commit read.
fn reap_locks<L, R: LockListing>(
    refs: &R,
    locks: LockKeys,
    now: LedgerTime,
) -> Result<Vec<LockLease>, LedgerReconcileError<L, R::Error>> {
    let mut reaped = Vec::new();
    for (repo_id, head) in locks {
        if let Some(lease) =
            reap_expired_lock(refs, repo_id, &head, now).map_err(LedgerReconcileError::Locks)?
        {
            reaped.push(lease);
        }
    }
    Ok(reaped)
}
