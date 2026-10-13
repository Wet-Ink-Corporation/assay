//! The `assay.decision/1` document `decide` produces (`docs/architecture/03-contracts.md` §5).

use serde::{Deserialize, Serialize};

use crate::decide::digest;
use crate::disposition::Disposition;
use crate::fingerprint::Fingerprint;
use crate::reason::ReasonCode;
use crate::vocabulary::closed_enum;

closed_enum! {
    /// The `schema` tag of a decision; a closed set of the versions this build reads.
    #[derive(Default)]
    pub enum DecisionSchema {
        /// `assay.decision/1`.
        #[default]
        V1 => "assay.decision/1",
    }
}

closed_enum! {
    /// The check conclusion a decision publishes (03 §5).
    pub enum Conclusion {
        /// The PR may merge.
        Success => "success",
        /// The PR may not merge.
        Failure => "failure",
        /// An owner override applies; passes for required checks.
        Neutral => "neutral",
        /// The LLM stage is deferred (rule 8a only); published as `in_progress`.
        Pending => "pending",
    }
}

/// One cause of the conclusion.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reason {
    /// The closed reason code.
    pub code: ReasonCode,
    /// Human-readable detail: the gate, the base ref, the role or the override scope.
    pub detail: String,
    /// The finding this reason is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<Fingerprint>,
}

/// How many findings of each severity the decision considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    /// Blockers.
    pub blocker: u64,
    /// Majors.
    pub major: u64,
    /// Minors.
    pub minor: u64,
    /// Nits.
    pub nit: u64,
}

/// What happened to one finding (03 §5 disposition codes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingDisposition {
    /// The finding's fingerprint.
    pub fp: Fingerprint,
    /// What happened to it.
    pub code: Disposition,
}

/// A gate finding an owner waived (04 §5 waive).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waived {
    /// The waived finding's fingerprint.
    pub fp: Fingerprint,
    /// Who waived it.
    pub by: String,
}

/// A reviewer role that ran degraded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DegradedRole {
    /// The role (`bugs`, `combined`).
    pub role: String,
    /// Why: its own cap, turns, timeout, or malformed output after a retry.
    pub reason: String,
}

/// An `assay.decision/1` document (03 §5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    /// The contract version.
    pub schema: DecisionSchema,
    /// `decision_key`: the idempotency key of this decision (02 §5).
    #[serde(rename = "decision_key")]
    pub key: String,
    /// The evidence this decision was made over (02 §5).
    pub evidence_key: String,
    /// The check conclusion.
    pub conclusion: Conclusion,
    /// Every cause, in rubric rule order; kept under an override.
    pub reasons: Vec<Reason>,
    /// Findings considered, by severity.
    pub counts: Counts,
    /// Verified blockers published while `llm_blockers` is advisory.
    pub advisory_blockers: Vec<Fingerprint>,
    /// What happened to individual findings.
    pub dispositions: Vec<FindingDisposition>,
    /// Gate findings removed by owner waivers.
    pub waived: Vec<Waived>,
    /// Required roles that ran degraded.
    pub degraded: Vec<DegradedRole>,
    /// The rubric version that made the decision.
    pub rubric_version: String,
    /// `sha256:<hex>` of the rubric and inputs `decide` read.
    pub inputs_digest: String,
}

impl Decision {
    /// The reasons digest an override must quote (04 §5): `sha256:<hex>` of the sorted reason
    /// codes, duplicates kept, without any `override` reason.
    #[must_use]
    pub fn reasons_digest(&self) -> String {
        digest::reasons_digest(&self.reasons)
    }
}
