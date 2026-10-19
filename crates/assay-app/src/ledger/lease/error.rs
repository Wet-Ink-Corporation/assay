//! Why a lease operation failed: the port's own failure apart from a lease assay refuses to
//! interpret (ERR-10, wi-b5affa). No error quotes a lease message.

use thiserror::Error;

use super::name::LeaseRef;
use crate::ledger::Corruption;

/// Why a lease could not be taken, read, released or consumed.
///
/// Every variant leaves the caller without the outcome: an unreadable lease fails `admit` like
/// an unreadable ledger, so the LLM-unavailable policy applies (06 §1, F16).
#[derive(Debug, Error)]
pub enum LeaseError<E> {
    /// The ledger repository's refs could not be read or written; a deletion or creation may
    /// or may not have happened.
    #[error("the ledger refs could not be read or written")]
    Refs(#[source] E),
    /// The ref's message breaks a rule of its contract; it was neither reaped nor taken over.
    #[error("lease {name} is refused: {problem}")]
    Corrupt {
        /// The ref.
        name: LeaseRef,
        /// The rule it breaks.
        problem: Corruption,
    },
    /// The lease to write would break a rule its own reader enforces (for example a `repo` that
    /// is not `owner/name`, or an expiry past 9999); nothing was written.
    #[error("the lease would be refused when read back: {problem}")]
    Invalid {
        /// The rule it would break.
        problem: Corruption,
    },
}
