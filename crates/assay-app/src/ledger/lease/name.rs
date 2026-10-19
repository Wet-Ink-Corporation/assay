//! The names of the lease refs, built only from typed parts so no ref name ever comes from
//! untrusted text.

use std::fmt;
use std::num::NonZeroU64;

use crate::config::CommitId;
use crate::prompt_bundle::Role;

/// How many model-running review jobs may run at once across every repository (A-03).
const SLOTS: u8 = 2;

/// One of the review slots, `refs/heads/slot-0` and `refs/heads/slot-1` (A-03, ADR-19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot(u8);

impl Slot {
    /// Every slot, in the order acquisition tries them.
    pub const ALL: [Self; 2] = [Self(0), Self(1)];

    /// The slot numbered `index`; `None` past the last one.
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if index < SLOTS {
            Some(Self(index))
        } else {
            None
        }
    }

    /// The slot's number, as `admit`'s `slot` output names it.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A lease ref of the ledger repository. Only ASCII letters, digits, `-` and `/` occur in it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseRef(String);

impl LeaseRef {
    /// `refs/heads/slot-<index>`: a review slot (45-minute TTL).
    #[must_use]
    pub fn slot(slot: Slot) -> Self {
        Self(format!("refs/heads/slot-{slot}"))
    }

    /// `refs/heads/lock-<repo_id>-<head_sha>`: the per-head publish lock (2-minute TTL, 02 §5).
    #[must_use]
    pub fn publish_lock(repo_id: NonZeroU64, head: &CommitId) -> Self {
        Self(format!("refs/heads/lock-{repo_id}-{head}"))
    }

    /// `refs/heads/retry/<repo_id>/<pr>/<head_sha>/<role>`: a one-shot retry grant (R3-14).
    #[must_use]
    pub fn retry_grant(repo_id: NonZeroU64, pr: NonZeroU64, head: &CommitId, role: Role) -> Self {
        Self(format!(
            "refs/heads/retry/{repo_id}/{pr}/{head}/{}",
            role.code()
        ))
    }

    /// The full ref name, `refs/heads/...`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LeaseRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
