//! What `decide` refuses to believe before the rubric runs: each refusal fails the run closed
//! with one reason (06 §6, F15, F18).

use assay_domain::{PrClass, Reason, ReasonCode, Tier};

use super::request::{JobResult, UpstreamJob};
use crate::plan::{ArtifactJob, BaseConfigError, FileListError};

/// Which of the PR's two refs moved since `plan` pinned them (R3-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Moved {
    /// The head is another commit.
    Head,
    /// The PR targets another base branch.
    BaseRef,
    /// Both.
    HeadAndBaseRef,
}

/// A fail-closed verdict reached before the rubric: published as `failure` with
/// [`Refusal::reason`].
#[derive(Debug)]
pub enum Refusal {
    /// The `plan` job did not succeed, so there is no plan to check.
    PlanNotSucceeded {
        /// How it ended.
        result: JobResult,
    },
    /// An upstream job was cancelled and no newer run exists for the head: a person cancelled
    /// it.
    Cancelled {
        /// The first cancelled job, in pipeline order.
        job: UpstreamJob,
    },
    /// An artifact's sha256 differs from its producing job's output digest (F18): it is never
    /// used.
    ArtifactDigest {
        /// The producing job.
        job: ArtifactJob,
        /// The file in its artifact.
        file: String,
    },
    /// An artifact `decide` cannot do without could not be read.
    ArtifactUnreadable {
        /// The producing job.
        job: ArtifactJob,
        /// The file in its artifact.
        file: String,
        /// Why.
        why: Unreadable,
    },
    /// A job's outputs do not describe what it ran.
    Outputs {
        /// The job.
        job: UpstreamJob,
        /// What is wrong with them.
        problem: OutputsProblem,
    },
    /// Z4's recomputation disagrees with the plan (F15).
    PlanMismatch(PlanMismatch),
}

/// Why an artifact could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unreadable {
    /// It is gone: expired, or deleted by a writer (not forgery, F20).
    Missing,
    /// It is larger than `decide` reads.
    TooLarge,
}

impl Unreadable {
    /// The reason detail's word for it.
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::Missing => "artifact-missing",
            Self::TooLarge => "document-too-large",
        }
    }
}

/// What is wrong with a job's outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputsProblem {
    /// A report names a plugin base config does not schedule in that job.
    UnknownPlugin,
    /// Two reports name one plugin.
    DuplicatePlugin,
    /// A skipped job reported a gate.
    SkippedJobReported,
    /// More documents than `decide` reads.
    TooManyDocuments,
    /// A document's file name is not a lowercase name and `.json`.
    FileName,
    /// Two documents share a file name.
    DuplicateFile,
}

impl OutputsProblem {
    const fn code(self) -> &'static str {
        match self {
            Self::UnknownPlugin => "unknown-plugin",
            Self::DuplicatePlugin => "duplicate-plugin",
            Self::SkippedJobReported => "skipped-job-reported",
            Self::TooManyDocuments => "too-many-documents",
            Self::FileName => "file-name",
            Self::DuplicateFile => "duplicate-file",
        }
    }
}

/// How the plan disagrees with Z4.
#[derive(Debug)]
pub enum PlanMismatch {
    /// The plan document fails `run-1.schema.json`.
    Invalid,
    /// Its artifact names are not one `assay-<job>-<nonce>` per job with distinct nonces, or its
    /// own differs from the `plan` job's output.
    ArtifactNames,
    /// A top-level field differs from Z4's recomputation; the first in key order.
    Field {
        /// The field's name, as Z4's own document spells it.
        name: String,
    },
    /// Z4 could not recompute the plan, where `plan` did.
    Recompute(PlanRefusal),
    /// `admit` degraded a run that is not a classless `full` (04 §2.4).
    Degrade {
        /// The recomputed tier.
        tier: Tier,
        /// The recomputed classes.
        classes: Vec<PrClass>,
    },
}

/// Why Z4's recomputation of the plan refused the PR.
#[derive(Debug)]
pub enum PlanRefusal {
    /// git did not list the changed files within its cap.
    DiffTooSlow,
    /// The git and REST file lists disagree.
    FileList(FileListError),
    /// Base `.assay/` was refused.
    BaseConfig(BaseConfigError),
}

impl PlanMismatch {
    fn detail(&self) -> String {
        match self {
            Self::Invalid => "the plan fails its schema".to_owned(),
            Self::ArtifactNames => "artifact_names".to_owned(),
            // The reason owns its detail; the refusal keeps its own name.
            Self::Field { name } => name.clone(),
            Self::Recompute(PlanRefusal::DiffTooSlow) => "diff-too-slow".to_owned(),
            Self::Recompute(PlanRefusal::FileList(_)) => "file-list".to_owned(),
            Self::Recompute(PlanRefusal::BaseConfig(_)) => "base-config".to_owned(),
            Self::Degrade { tier, .. } => format!("tier_degraded_from on a {} run", tier.code()),
        }
    }
}

impl Refusal {
    /// The reason the check is failed with: `plan-mismatch` for a plan Z4 disagrees with,
    /// `gate-error` for everything else.
    #[must_use]
    pub fn reason(&self) -> Reason {
        let (code, detail) = match self {
            Self::PlanNotSucceeded { result } => {
                (ReasonCode::GateError, format!("plan: {}", result.code()))
            }
            Self::Cancelled { job } => {
                (ReasonCode::GateError, format!("{}: cancelled", job.name()))
            }
            Self::ArtifactDigest { job, file } => (
                ReasonCode::GateError,
                format!("artifact-digest: {}/{file}", job.name()),
            ),
            Self::ArtifactUnreadable { job, file, why } => (
                ReasonCode::GateError,
                format!("{}: {}/{file}", why.code(), job.name()),
            ),
            Self::Outputs { job, problem } => (
                ReasonCode::GateError,
                format!("outputs: {}: {}", job.name(), problem.code()),
            ),
            Self::PlanMismatch(mismatch) => (ReasonCode::PlanMismatch, mismatch.detail()),
        };
        Reason {
            code,
            detail,
            fp: None,
        }
    }
}

impl From<PlanMismatch> for Refusal {
    fn from(mismatch: PlanMismatch) -> Self {
        Self::PlanMismatch(mismatch)
    }
}
