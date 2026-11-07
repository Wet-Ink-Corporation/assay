//! The pending stamp `open` writes into the verdict check's `external_id`:
//! `pending:<run_id>:<seq>` (02 §5, R3-09).

use std::fmt;
use std::num::NonZeroU64;

/// `pending:<run_id>:<seq>`: the run that owns the verdict check until `decide` replaces the
/// stamp with its decision key, and the number of the PR's latest push record (0 before the
/// first one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PendingStamp {
    run_id: NonZeroU64,
    seq: u64,
}

impl PendingStamp {
    /// The stamp of workflow run `run_id` after push record `seq`.
    #[must_use]
    pub const fn new(run_id: NonZeroU64, seq: u64) -> Self {
        Self { run_id, seq }
    }

    /// The stamp `external_id` holds, exactly as [`fmt::Display`] writes one: `pending:`, a run
    /// id of ASCII digits without a leading zero, `:`, and a number of ASCII digits. `None` for
    /// anything else, such as a decision key.
    #[must_use]
    pub fn parse(external_id: &str) -> Option<Self> {
        let (run_id, seq) = external_id.strip_prefix("pending:")?.split_once(':')?;
        let stamp = Self {
            run_id: digits(run_id)?.parse().ok()?,
            seq: digits(seq)?.parse().ok()?,
        };
        // Only the canonical form names a run: `pending:030:2` is not run 30's stamp.
        (stamp.to_string() == external_id).then_some(stamp)
    }

    /// The workflow run that wrote it.
    #[must_use]
    pub const fn run_id(self) -> NonZeroU64 {
        self.run_id
    }

    /// The number of the push record it was written after.
    #[must_use]
    pub const fn seq(self) -> u64 {
        self.seq
    }
}

/// `text` when it is one or more ASCII digits.
fn digits(text: &str) -> Option<&str> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())).then_some(text)
}

impl fmt::Display for PendingStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pending:{}:{}", self.run_id, self.seq)
    }
}
