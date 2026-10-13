//! The `assay.findings/1` document one gate or one reviewer role writes
//! (`docs/architecture/03-contracts.md` §3).

use serde::{Deserialize, Serialize};

use crate::finding::Finding;
use crate::vocabulary::closed_enum;

closed_enum! {
    /// The `schema` tag of a findings document; a closed set of the versions this build reads.
    #[derive(Default)]
    pub enum FindingsSchema {
        /// `assay.findings/1`.
        #[default]
        V1 => "assay.findings/1",
    }
}

closed_enum! {
    /// Who produced a findings document.
    pub enum SourceKind {
        /// A mechanical gate (temper, Weigh-In, an `exec` gate).
        Gate => "gate",
        /// A model reviewer role.
        Reviewer => "reviewer",
    }
}

closed_enum! {
    /// A gate's own conclusion (03 §2); reviewers have none.
    pub enum GateConclusion {
        /// The gate passed.
        Pass => "pass",
        /// The gate failed.
        Fail => "fail",
        /// The gate had nothing to say.
        Neutral => "neutral",
    }
}

/// The producer of a findings document.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Gate or reviewer.
    pub kind: SourceKind,
    /// Plugin or reviewer name (`temper`, `claude`).
    pub name: String,
    /// Pinned version (`git:<sha>`, `oci:sha256:…`).
    pub version: String,
    /// The reviewer role (`bugs`); required for reviewers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The run this document belongs to.
    pub run_key: String,
}

/// Counters a producer reports about its own work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stats {
    /// Files the producer read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_read: Option<u64>,
}

/// An `assay.findings/1` document (03 §3).
///
/// A contract record: the schema, checked on read, requires `conclusion` from gates and forbids
/// it from reviewers (a reviewer cannot say "approve").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingsDocument {
    /// The contract version.
    pub schema: FindingsSchema,
    /// Who produced the document.
    pub source: Source,
    /// The gate's conclusion; absent for reviewers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<GateConclusion>,
    /// The findings, possibly none.
    pub findings: Vec<Finding>,
    /// The producer's counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
}
