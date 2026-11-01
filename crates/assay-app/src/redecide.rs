//! decide-only's re-decision from stored evidence (#61, `docs/architecture/04-pipeline.md` §6):
//! a command, label or poll re-decides a PR's head from the evidence bundle the ledger points
//! at, never re-running a gate or a model.
//!
//! This module holds the evidence read ([`read_evidence`]), the re-decision's inputs
//! ([`redecision_inputs`]) over the snapshot
//! [`redecided_state`](crate::redecided_state) takes from the bundle's recorded facts, and the
//! decision bundle decide-only persists (D5, `assay.run-decision/1`): built by
//! [`prepare_decision`] and read by [`read_decision_bundle`], one file, [`DECISION_FILE`], in an
//! artifact named `assay-decision-<pr>-<nonce>`, one per PR, since one run may decide several.
//! [`replay_chain`] replays such a bundle through the evidence bundle it names (J10).

mod bundle;
mod chain;
mod evidence;
mod inputs;
mod read;

pub use self::bundle::{
    DecisionReadBack, DecisionTarget, DecisionTrigger, PrepareDecisionError, Redecided,
    TriggerCommand, prepare_decision,
};
pub use self::chain::{
    ChainError, ChainMatched, ChainMismatch, ChainPorts, ChainReplayed, ChainRequest, InputsField,
    replay_chain,
};
pub use self::evidence::{
    Absence, EvidenceBundle, EvidenceError, EvidenceRead, EvidenceRequest, Forgery, read_evidence,
};
pub use self::inputs::{RedecisionRequest, redecision_inputs};
pub use self::read::{DecisionBundle, DecisionBundleError, read_decision_bundle};

/// The decision bundle's file in its artifact (`assay.run-decision/1`, D5).
pub const DECISION_FILE: &str = "run-decision.json";

/// The most bytes a decision bundle may hold: it holds no document text, only the decision
/// inputs, the decision, its events and the GitHub snapshot.
pub const MAX_DECISION_BUNDLE_BYTES: usize = 8 << 20;
