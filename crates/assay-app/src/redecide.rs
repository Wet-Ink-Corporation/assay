//! decide-only's re-decision from stored evidence (#61, `docs/architecture/04-pipeline.md` §6):
//! a command, label or poll re-decides a PR's head from the evidence bundle the ledger points
//! at, never re-running a gate or a model.
//!
//! This module holds the evidence read ([`read_evidence`]) and the names of the decision bundle
//! decide-only persists (D5): one file, [`DECISION_FILE`], in an artifact named
//! `assay-decision-<pr>-<nonce>`, one per PR, since one run may decide several.

mod evidence;

pub use self::evidence::{
    Absence, EvidenceBundle, EvidenceError, EvidenceRead, EvidenceRequest, Forgery, read_evidence,
};

/// The decision bundle's file in its artifact (`assay.run-decision/1`, D5).
pub const DECISION_FILE: &str = "run-decision.json";

/// The most bytes a decision bundle may hold: it holds no document text, only the decision
/// inputs, the decision, its events and the GitHub snapshot.
pub const MAX_DECISION_BUNDLE_BYTES: usize = 8 << 20;
