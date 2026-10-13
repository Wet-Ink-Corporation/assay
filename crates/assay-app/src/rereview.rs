//! Re-review (`docs/architecture/04-pipeline.md` §7, R-06): a new push is a new run.
//!
//! The specialists always review the full cumulative diff, and the increment since the last
//! completed run is only marked ([`review_target`]). Findings open from earlier runs are re-judged,
//! not string-matched: they are read as untrusted text ([`read_carried`]), kept only while the
//! previous head is an ancestor of head (a rebase or force-push resets them;
//! dismissals are ledger records outside this set and stay), their evidence is searched for
//! again at head ([`recheck_anchors`]), the blind verifier asks whether each defect is still
//! present ([`verify`](crate::verify)), and [`settle_carried`] says what becomes of each.

mod anchor;
mod carried;
mod settle;
mod target;

pub use anchor::recheck_anchors;
pub use carried::{
    CarriedAnchor, CarriedField, CarriedFinding, CarriedParts, CarriedProblem, CarriedRefusal,
    CarryRefused, MAX_CARRIED, MAX_CARRIED_PATH_BYTES, read_carried,
};
pub use settle::{CarriedOutcome, CarriedState, NotReverified, settle_carried};
pub use target::{
    Ancestry, Carry, CarryReset, Cumulative, HistoryStep, Increment, MAX_RANGE_DIFF_BYTES,
    PriorHistory, RangeDiff, RangeDiffCommits, RangeDiffListing, ReviewTarget, TargetError,
    TargetRequest, review_target,
};
