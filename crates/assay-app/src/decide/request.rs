//! What the pipeline's `decide` job is given besides the PR's objects: the outputs and results
//! of the jobs it needs, which steps PR code cannot reach wrote (02 §1). They are trusted; the
//! artifacts they name are not until their digests match.

use assay_domain::GateConclusion;

use crate::config::Sha256Digest;
use crate::gate::GateError;
use crate::plan::PlanRequest;

/// How an upstream job ended, as `needs.<job>.result` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobResult {
    /// Every step succeeded.
    Success,
    /// A step failed, or the job ran past its timeout.
    Failure,
    /// The job was cancelled, by a newer run's `cancel-in-progress` or by a person.
    Cancelled,
    /// The job did not run (its condition was false or a job it needs failed).
    Skipped,
}

impl JobResult {
    /// The result as GitHub writes it.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }
}

/// A job `decide` needs (02 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UpstreamJob {
    /// `plan`.
    Plan,
    /// `gates-static`.
    GatesStatic,
    /// `gates-lint`.
    GatesLint,
    /// `gates-test`.
    GatesTest,
    /// `admit`.
    Admit,
    /// `review`.
    Review,
    /// `release`.
    Release,
}

impl UpstreamJob {
    /// The job's name in the workflow.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::GatesStatic => "gates-static",
            Self::GatesLint => "gates-lint",
            Self::GatesTest => "gates-test",
            Self::Admit => "admit",
            Self::Review => "review",
            Self::Release => "release",
        }
    }
}

/// The `plan` job's outputs: its artifact and the digest of the `run.json` in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanJob {
    /// How the job ended.
    pub result: JobResult,
    /// The plan artifact's name, `assay-plan-<nonce>`.
    pub artifact: String,
    /// The sha256 of the `run.json` the job uploaded.
    pub document: Sha256Digest,
}

/// One gates job's outputs: a report per plugin it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateJobOutputs {
    /// How the job ended.
    pub result: JobResult,
    /// What the job reported for each plugin scheduled in it, by plugin name.
    pub gates: Vec<ReportedGate>,
}

/// What a gates job reported for one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedGate {
    /// The plugin's name in base `.assay/plugins.toml`.
    pub plugin: String,
    /// How the gate ended.
    pub reported: Reported,
}

/// How a gate ended in its job (03 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reported {
    /// It ran cleanly and uploaded `<plugin>.json` in the job's artifact.
    Concluded {
        /// The conclusion assay drew from the exit code and the document.
        conclusion: GateConclusion,
        /// The sha256 of the uploaded document.
        document: Sha256Digest,
    },
    /// It is a gate error; nothing of it is uploaded.
    Error(GateError),
}

/// Whether `admit` degraded the run's tier for budget (04 §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TierDegrade {
    /// No degrade.
    None,
    /// `budget.tier_degraded_from = "full"`: a non-trust-path `full` run reviewed as `lite`.
    FullToLite,
}

/// The `admit` job's outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmitJob {
    /// How the job ended; a degrade counts only from a successful `admit`.
    pub result: JobResult,
    /// Its tier degrade.
    pub degrade: TierDegrade,
}

/// One document of the `review` artifact and its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDocument {
    /// The file in the artifact: a lowercase name of letters, digits and `-`, then `.json`.
    pub file: String,
    /// Its sha256.
    pub digest: Sha256Digest,
}

/// The `review` job's outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewJob {
    /// How the job ended.
    pub result: JobResult,
    /// Every document it uploaded.
    pub documents: Vec<ArtifactDocument>,
}

/// Everything `decide` is given besides the PR's objects and GitHub's live state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecideRequest {
    /// What Z4 recomputes the plan from: the triggering event's PR and pinned SHAs, this run,
    /// Z4's own REST file list, the package sets and include targets `gates-static` reported,
    /// and the prompt bundle hash.
    pub plan: PlanRequest,
    /// The `plan` job.
    pub plan_job: PlanJob,
    /// The `gates-static` job.
    pub gates_static: GateJobOutputs,
    /// The `gates-lint` job.
    pub gates_lint: GateJobOutputs,
    /// The `gates-test` job.
    pub gates_test: GateJobOutputs,
    /// The `admit` job.
    pub admit: AdmitJob,
    /// The `review` job.
    pub review: ReviewJob,
    /// How the `release` job ended.
    pub release: JobResult,
}

impl DecideRequest {
    /// Every upstream job's result, in pipeline order.
    pub(super) const fn results(&self) -> [(UpstreamJob, JobResult); 7] {
        [
            (UpstreamJob::Plan, self.plan_job.result),
            (UpstreamJob::GatesStatic, self.gates_static.result),
            (UpstreamJob::GatesLint, self.gates_lint.result),
            (UpstreamJob::GatesTest, self.gates_test.result),
            (UpstreamJob::Admit, self.admit.result),
            (UpstreamJob::Review, self.review.result),
            (UpstreamJob::Release, self.release),
        ]
    }

    /// The first upstream job that was cancelled, if any.
    pub(super) fn cancelled(&self) -> Option<UpstreamJob> {
        self.results()
            .into_iter()
            .find(|(_, result)| *result == JobResult::Cancelled)
            .map(|(job, _)| job)
    }
}
