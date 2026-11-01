//! `decide`'s inputs and outputs (03 §4a, #60 ST-14, D10 A): what it reads of every upstream
//! job ([`DecideNeeds`]), and what it reports for the upload step ([`DecideOutputs`]).

use assay_app::{
    AdmitJob, AdmitOutcome, BuildJob, Deferred, GateJob, GateJobOutputs, ReviewJob, ReviewStage,
    Sha256Digest, TierDegrade,
};
use assay_domain::{Aeu, NotRunReason, Tier};

use super::{Identity, PlanOutputs, Read, ReviewEnd, flag_word, gates_from_needs};
use crate::ci::{JobName, JobResult, Needs, NeedsError, OutputName};

/// The jobs `decide` needs: every job of the pipeline before it.
pub const DECIDE_NEEDS: [JobName; 9] = JobName::ALL;

/// What `decide` reports: whether it wrote `run-inputs.json` for the upload step, and its
/// sha256 when it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideOutputs {
    /// No bundle: the run was superseded, yields to a newer one, or was refused.
    NoBundle,
    /// The bundle written, by the sha256 of its exact bytes.
    Bundle {
        /// Its sha256.
        digest: Sha256Digest,
    },
}

impl DecideOutputs {
    /// decide's outputs, in [`OutputName::DECIDE`]'s order: `bundle` (`true` or `false`) and,
    /// for a bundle only, `bundle_digest`.
    #[must_use]
    pub fn outputs(&self) -> Vec<(OutputName, String)> {
        match self {
            Self::NoBundle => vec![(OutputName::Bundle, flag_word(false).to_owned())],
            Self::Bundle { digest } => vec![
                (OutputName::Bundle, flag_word(true).to_owned()),
                (OutputName::BundleDigest, digest.to_string()),
            ],
        }
    }
}

/// How the `plan` job ended, for `decide`: its outputs only when it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanEnd {
    /// It succeeded with these outputs.
    Succeeded(PlanOutputs),
    /// It ended otherwise; its outputs are not read.
    NotSucceeded(JobResult),
}

/// Everything `decide` reads of its upstream jobs (03 §4a): guard's identity, plan's outputs,
/// what each gates job reported, admit's admission, the review's report and release's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecideNeeds {
    /// guard's identity of the PR (I2).
    pub identity: Identity,
    /// How plan ended.
    pub plan: PlanEnd,
    /// `gates-static`.
    pub gates_static: GateJobOutputs,
    /// `gates-lint`.
    pub gates_lint: GateJobOutputs,
    /// `gates-test`.
    pub gates_test: GateJobOutputs,
    /// `admit`.
    pub admit: AdmitJob,
    /// `review`.
    pub review: ReviewJob,
    /// How `release` ended.
    pub release: assay_app::JobResult,
}

impl DecideNeeds {
    /// What `decide` reads of `needs`, which holds exactly [`DECIDE_NEEDS`].
    ///
    /// guard must have succeeded. Each other job's outputs are read only when it succeeded
    /// (admit's whatever it ended with is not believed: an `admit` that did not succeed reads
    /// as an unavailable LLM in `decide`); its result is passed on for `decide` to judge.
    ///
    /// # Errors
    ///
    /// [`NeedsError`] for a missing job, guard's identity, plan's outputs or a gates job's
    /// `gates` refused as their own readers refuse them, and for admit's and the review's
    /// outputs as [`admit_job`] and [`ReviewEnd::from_needs`] refuse them.
    pub fn from_needs(needs: &Needs) -> Result<Self, NeedsError> {
        let identity = Identity::from_needs(needs)?;
        let plan = match result(needs, JobName::Plan)? {
            JobResult::Success => PlanEnd::Succeeded(PlanOutputs::from_needs(needs)?),
            other => PlanEnd::NotSucceeded(other),
        };
        let planned = match &plan {
            PlanEnd::Succeeded(outputs) => Some(outputs),
            PlanEnd::NotSucceeded(_) => None,
        };
        Ok(Self {
            identity,
            gates_static: gate_job(needs, GateJob::Static)?,
            gates_lint: gate_job(needs, GateJob::Build(BuildJob::Lint))?,
            gates_test: gate_job(needs, GateJob::Build(BuildJob::Test))?,
            admit: admit_job(needs, planned)?,
            review: ReviewEnd::from_needs(needs)?.into(),
            release: result(needs, JobName::Release)?.into(),
            plan,
        })
    }
}

/// How `job` ended in `needs`.
fn result(needs: &Needs, job: JobName) -> Result<JobResult, NeedsError> {
    needs
        .job(job)
        .map(crate::ci::Upstream::result)
        .ok_or(NeedsError::MissingJob { job })
}

/// What gates job `job` reported: its reports when it succeeded, none otherwise.
fn gate_job(needs: &Needs, job: GateJob) -> Result<GateJobOutputs, NeedsError> {
    let name = match job {
        GateJob::Static => JobName::GatesStatic,
        GateJob::Build(BuildJob::Lint) => JobName::GatesLint,
        GateJob::Build(BuildJob::Test) => JobName::GatesTest,
    };
    let ended = result(needs, name)?;
    let gates = match ended {
        JobResult::Success => gates_from_needs(needs, job)?,
        JobResult::Failure | JobResult::Cancelled | JobResult::Skipped => Vec::new(),
    };
    Ok(GateJobOutputs {
        result: ended.into(),
        gates,
    })
}

/// What `admit` admitted, read from its outputs in `needs` against plan's `planned` outputs.
///
/// An `admit` that did not succeed is not read: `decide` reads it as an unavailable LLM
/// whatever it admitted, and its degrade is not accepted, so it is passed on with no degrade.
/// One that succeeded must report an admission its plan allows: `no-spend` exactly when plan
/// gives the run no review (its reason the draft flag, else a tier with no LLM stage), and
/// `deferral` with `deferred` only.
///
/// # Errors
///
/// [`NeedsError::MissingJob`] without `admit`; [`NeedsError::NotSucceeded`] for plan when admit
/// succeeded after a plan that did not; [`NeedsError::UnexpectedOutput`] for an output that is
/// not admit's; [`NeedsError::MissingOutput`] for a required one absent; and
/// [`NeedsError::Output`] for an `admission`, `deferral`, `tier`, `degrade` or `review` that
/// does not parse or contradicts the plan or the admission.
pub fn admit_job(needs: &Needs, planned: Option<&PlanOutputs>) -> Result<AdmitJob, NeedsError> {
    let job = JobName::Admit;
    let read = Read::any_result(needs, job, &OutputName::ADMIT)?;
    let ended = result(needs, job)?;
    if ended != JobResult::Success {
        return Ok(AdmitJob {
            result: ended.into(),
            degrade: TierDegrade::None,
            // Not read: `decide` reads an admit that did not succeed as an unavailable LLM,
            // whatever it admitted, and `Reused` reads the same way.
            admission: AdmitOutcome::Reused,
        });
    }
    let plan = planned.ok_or(NeedsError::NotSucceeded {
        job: JobName::Plan,
        result: result(needs, JobName::Plan)?,
    })?;
    let degrade = read.parsed(OutputName::Degrade, |text| match text {
        "none" => Some(TierDegrade::None),
        "full" => Some(TierDegrade::FullToLite),
        _ => None,
    })?;
    let admission = read.required(OutputName::Admission)?;
    let deferral = read.optional(OutputName::Deferral);
    let reviews = plan.review();
    let outcome = match (admission, reviews) {
        ("no-spend", false) => AdmitOutcome::NoSpend(if plan.draft() {
            NotRunReason::Draft
        } else {
            NotRunReason::NoLlmTier
        }),
        ("reused", true) => AdmitOutcome::Reused,
        ("deferred", true) => AdmitOutcome::Deferred(
            [Deferred::Budget, Deferred::SlotBusy]
                .into_iter()
                .find(|known| Some(known.deferral().code()) == deferral)
                .ok_or_else(|| read.invalid(OutputName::Deferral))?,
        ),
        ("spend", true) => AdmitOutcome::Spend {
            tier: read.parsed(OutputName::Tier, |text| {
                Tier::ALL.into_iter().find(|tier| tier.code() == text)
            })?,
        },
        _ => return Err(read.invalid(OutputName::Admission)),
    };
    if deferral.is_some() && !matches!(outcome, AdmitOutcome::Deferred(_)) {
        return Err(read.invalid(OutputName::Deferral));
    }
    let spends = matches!(outcome, AdmitOutcome::Spend { .. });
    if read.parsed(OutputName::Review, super::parse_flag)? != spends {
        return Err(read.invalid(OutputName::Review));
    }
    Ok(AdmitJob {
        result: ended.into(),
        degrade,
        admission: outcome,
    })
}

impl From<ReviewEnd> for ReviewJob {
    /// The review as `decide` reads it: what a successful one reported, and for any other its
    /// result alone (it did not run its stage and spent nothing `decide` shows). No review
    /// writes documents until M2.
    fn from(end: ReviewEnd) -> Self {
        let (result, report) = match end {
            ReviewEnd::Reported(report) => (assay_app::JobResult::Success, Some(report)),
            ReviewEnd::Failed => (assay_app::JobResult::Failure, None),
            ReviewEnd::Cancelled => (assay_app::JobResult::Cancelled, None),
            ReviewEnd::Skipped => (assay_app::JobResult::Skipped, None),
        };
        Self {
            result,
            stage: report.map_or(ReviewStage::NotRun, |report| report.stage),
            cost: report.map_or(Aeu::from_micros(0), |report| report.cost),
            documents: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
