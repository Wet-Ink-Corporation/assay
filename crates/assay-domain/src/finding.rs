//! One finding of `assay.findings/1` and its closed vocabularies
//! (`docs/architecture/03-contracts.md` §3).

use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use crate::severity::{Marker, Severity};
use crate::vocabulary::closed_enum;

closed_enum! {
    /// What kind of defect a finding reports. Part of the fingerprint, so two runs that describe
    /// the same defect in different words still collide (03 §3).
    pub enum Class {
        /// Wrong behaviour.
        Logic => "logic",
        /// Failures lost, hidden or mishandled.
        ErrorHandling => "error-handling",
        /// Races, deadlocks, ordering.
        Concurrency => "concurrency",
        /// Leaks and unbounded use of memory, handles or time.
        Resource => "resource",
        /// A security weakness.
        Security => "security",
        /// A public interface that is hard to use correctly.
        ApiDesign => "api-design",
        /// A dependency pointing the wrong way between layers.
        Layering => "layering",
        /// Missing or weak tests.
        Tests => "tests",
        /// Missing or wrong documentation.
        Docs => "docs",
        /// A breach of a stated rule or policy.
        Compliance => "compliance",
        /// A dependency problem.
        Deps => "deps",
    }
}

closed_enum! {
    /// Whether a finding quotes faulty code or reports something missing.
    pub enum FindingKind {
        /// The quoted code is wrong.
        Defect => "defect",
        /// Something is missing; nothing to quote, so the finding anchors on the enclosing
        /// function and states `expected_behavior`.
        Omission => "omission",
    }
}

closed_enum! {
    /// How the diff introduced a finding.
    pub enum IntroducedBy {
        /// The changed lines themselves.
        Diff => "diff",
        /// A changed symbol breaks an unchanged caller, named in `caller`.
        Interaction => "interaction",
    }
}

closed_enum! {
    /// The side of the diff a line number refers to, as GitHub names them.
    pub enum Side {
        /// The head side.
        Right => "RIGHT",
        /// The base side.
        Left => "LEFT",
    }
}

closed_enum! {
    /// How sure the reviewer is of a finding.
    pub enum Confidence {
        /// Sure.
        High => "high",
        /// Fairly sure.
        Medium => "medium",
        /// Unsure.
        Low => "low",
    }
}

/// The unchanged caller a changed symbol affects, required when `introduced_by` is
/// `interaction`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caller {
    /// Path of the caller's file.
    pub file: String,
    /// Line of the call.
    pub line: NonZeroU64,
    /// Verbatim span of the call, at most 400 characters.
    pub evidence: String,
}

/// One finding from a gate or a reviewer role (03 §3).
///
/// A contract record: its length caps and conditional requirements (`expected_behavior` for an
/// omission, `caller` for an interaction) are enforced by `findings-1.schema.json` when the
/// document is read, not by this type. There is no `fp` and no `group_id`: producers never
/// supply them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// What kind of defect this is.
    pub class: Class,
    /// Gate: the plugin's rule id; reviewer: an optional rule-id citation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// Defect or omission.
    pub kind: FindingKind,
    /// How bad it is, as the producer claims.
    pub severity: Severity,
    /// How the diff introduced it, when the producer says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introduced_by: Option<IntroducedBy>,
    /// The producer's claim that the diff did not introduce it; Z4 verifies it against the diff.
    #[serde(default)]
    pub pre_existing: bool,
    /// Path of the file.
    pub file: String,
    /// First line.
    pub line: NonZeroU64,
    /// Last line, when the finding spans several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<NonZeroU64>,
    /// Side of the diff the lines refer to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    /// Verbatim span (at most 400 characters); for an omission, the function signature.
    pub evidence: String,
    /// What should happen instead; required for an omission (at most 300 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_behavior: Option<String>,
    /// How the defect is reached; expected for `major` and above (at most 300 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// The affected unchanged caller; required for an interaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Caller>,
    /// Plain-text title, at most 100 characters.
    pub title: String,
    /// Restricted markdown body, at most 1,200 characters.
    pub body: String,
    /// Suggested fix, at most 600 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// How sure the reviewer is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// A short root-cause label; the coordinator assigns the authoritative group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_hint: Option<String>,
}

impl Finding {
    /// The marker this finding is displayed with (03 §3), from its severity and its
    /// `pre_existing` flag.
    #[must_use]
    pub const fn marker(&self) -> Marker {
        Marker::of(self.severity, self.pre_existing)
    }
}
