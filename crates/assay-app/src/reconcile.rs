//! The reconcile sweeps (`docs/architecture/06-operations.md` §4, §1, F17, F21; R3-05, R3-06,
//! R3-16): every 6 hours, each consumer repository closes its own stuck and superseded verdict
//! checks ([`reconcile_checks`]), and one designated job in the ledger repository reaps expired
//! leases and back-fills the cost records `release` never wrote ([`reconcile_ledger`]).
//!
//! The split is the invariant: only [`reconcile_ledger`] reaps slot leases and back-fills, so
//! consumer repositories never race to back-fill the same expired lease (R3-16). Both refuse a
//! re-run (`run_attempt != 1`) before any port is called (R3-05, 02 §1).

mod checks;
mod ledger;
mod locks;
mod outcome;
mod ports;

pub use checks::{
    ASSAY_WORKFLOWS, ChecksReconcileRequest, LLM_PENDING_LABEL, MAX_HEAD_CHECKS, MAX_HEAD_RUNS,
    MAX_OPEN_PULLS, MAX_SUPERSEDED_HEADS, ReconcilePorts, STUCK_AFTER, reconcile_checks,
};
pub use ledger::{
    LedgerReconcileError, LedgerReconcileRequest, LedgerReconciled, reconcile_ledger,
};
pub use locks::{LockListing, MAX_PUBLISH_LOCKS};
pub use outcome::{
    ChecksReconcileError, ChecksReconciled, ClosedCheck, Closure, Listing, Skip, SkippedCheck,
};
pub use ports::{LiveRun, OpenPull, PullHead, ReconcileCheck, ReconcileForge};
