//! Leases on refs of the ledger repository (06 §1, 02 §5, ADR-19, R3-06, R3-09, R3-14): the
//! review slots `refs/heads/slot-N`, the per-head publish lock and the one-shot retry grants.
//!
//! One primitive serves all three. A ref is taken by creating it at a new commit whose message
//! is the lease document, which fails when the ref exists (A-26); it is released, reaped or
//! consumed only by deleting it on condition that it still points at the commit read or created
//! (A-33), so a stale holder never deletes a newer holder's lease. The documents are contracts
//! read back strictly, like the ledger's files (T23, wi-b5affa): a lease that cannot be read
//! exactly is neither reaped nor taken over, and the read fails.

mod document;
mod error;
mod grant;
mod lock;
mod name;
mod refs;
mod slot;
mod take;

pub use error::LeaseError;
pub use grant::{
    Consumed, GrantKey, Granted, RetryGrant, consume_grant, grant_retry, pending_grant,
};
pub use lock::{
    LOCK_TTL, LockAcquired, LockHold, LockLease, LockRequest, acquire_lock, release_lock,
};
pub use name::{LeaseRef, Slot};
pub use refs::{LedgerRefs, MAX_LEASE_MESSAGE_BYTES, RefCreation, RefDeletion, RefTip};
pub use slot::{
    MAX_REVIEW_TIMEOUT, SLOT_TTL, SlotAcquired, SlotAcquisition, SlotHold, SlotLease, SlotRequest,
    SlotView, acquire_slot, read_slots, reap_expired_slots, release_slot,
};
pub use take::{LeaseState, Released};
