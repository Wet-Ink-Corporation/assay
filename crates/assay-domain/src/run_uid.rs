//! The identity of one workflow run (`docs/architecture/03-contracts.md` §4, 02 §5).

use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

/// One workflow run, `(repo_id, run_id, run_attempt)`: keys leases, ledger records and the
/// pending stamp. GitHub numbers all three from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunUid {
    /// The repository's numeric GitHub id.
    pub repo_id: NonZeroU64,
    /// The workflow run id.
    pub run_id: NonZeroU64,
    /// The attempt of that run.
    pub run_attempt: NonZeroU64,
}
