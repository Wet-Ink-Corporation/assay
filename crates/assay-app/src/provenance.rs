//! Run provenance (J4, F19, `docs/architecture/02-architecture.md` §5): which workflow run
//! wrote a bundle a ledger pointer names, and whether assay believes it.
//!
//! A pointer is written by the App, but the artifact it names is uploaded by a workflow run,
//! and only the installed workflow file of the default branch runs with assay's trust. A bundle
//! is therefore believed only for attempt 1 of the expected workflow file of this repository,
//! started by one of the events that workflow runs for, with this repository as its head
//! repository and, for every event but `pull_request_target`, on the default branch. Anything
//! else is a [`ProvenanceRefusal`], a closed code with nothing read from GitHub in it
//! (wi-b5affa).

use std::num::NonZeroU64;

use assay_domain::RunUid;
use thiserror::Error;

/// The workflow runs GitHub reports, read by id. Implemented by the forge adapter.
pub trait RunProvenance {
    /// Why the run could not be read.
    type Error: std::error::Error;

    /// The facts of workflow run `run_id` of this repository; `None` when GitHub has no such
    /// run.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the run cannot be read, or GitHub answers for another run.
    fn workflow_run(&self, run_id: NonZeroU64) -> Result<Option<WorkflowRunFacts>, Self::Error>;
}

/// What GitHub reports of a workflow run, as provenance reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunFacts {
    /// The run's id.
    pub run_id: NonZeroU64,
    /// The repository the run belongs to.
    pub repo_id: NonZeroU64,
    /// The workflow file the run executed, as GitHub reports it (`path`).
    pub path: String,
    /// The event that started the run.
    pub event: RunEvent,
    /// The branch the run's workflow file was read from (`head_branch`).
    pub head_branch: String,
    /// The run's head repository; `None` when GitHub reports none (a deleted fork).
    pub head_repo: Option<NonZeroU64>,
    /// The run's attempt.
    pub run_attempt: NonZeroU64,
}

/// The event that started a run. Closed: an event assay never runs for is [`RunEvent::Other`],
/// carrying nothing of what GitHub said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunEvent {
    /// `pull_request_target`.
    PullRequestTarget,
    /// `workflow_dispatch`.
    WorkflowDispatch,
    /// `issue_comment`.
    IssueComment,
    /// `schedule`.
    Schedule,
    /// Any other event, `pull_request` included.
    Other,
}

/// The workflows that write bundles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Workflow {
    /// The pipeline, which writes the evidence bundle (`assay.run-inputs/1`).
    Pipeline,
    /// decide-only, which writes decision bundles (`assay.run-decision/1`, D5).
    DecideOnly,
}

impl Workflow {
    /// The workflow's file, exactly as GitHub reports a run's `path`.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::Pipeline => ".github/workflows/assay.yml",
            Self::DecideOnly => ".github/workflows/assay-decide.yml",
        }
    }

    /// Whether the workflow runs for `event`.
    const fn runs_for(self, event: RunEvent) -> bool {
        match self {
            Self::Pipeline => matches!(
                event,
                RunEvent::PullRequestTarget | RunEvent::WorkflowDispatch
            ),
            Self::DecideOnly => matches!(
                event,
                RunEvent::IssueComment | RunEvent::PullRequestTarget | RunEvent::Schedule
            ),
        }
    }
}

/// The run a pointer names and what it must be.
#[derive(Debug, Clone, Copy)]
pub struct ExpectedRun<'a> {
    /// The workflow that writes the bundle.
    pub workflow: Workflow,
    /// The run the pointer names, with the repository it was recorded for.
    pub run: RunUid,
    /// The repository's default branch.
    pub default_branch: &'a str,
}

/// Why a run is not one whose bundle assay believes. Each is a forgery of the pointer (F19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum ProvenanceRefusal {
    /// GitHub has no such run: an App-written pointer names no run.
    #[error("the pointer names a run GitHub does not have")]
    Missing,
    /// GitHub reports another run than the pointer names.
    #[error("the run is another than the pointer names")]
    OtherRun,
    /// The run belongs to another repository.
    #[error("the run belongs to another repository")]
    OtherRepository,
    /// The run executed another workflow file than the expected one.
    #[error("the run executed another workflow file")]
    Workflow,
    /// The run was started by an event the workflow does not run for.
    #[error("the run was started by an event the workflow does not run for")]
    Event,
    /// The run was started on a branch other than the default branch.
    #[error("the run was started on a branch other than the default branch")]
    Branch,
    /// The run's head repository is not this repository, or there is none.
    #[error("the run's head repository is not this repository")]
    HeadRepository,
    /// The run or the pointer is not attempt 1 (R3-05).
    #[error("the run is not attempt 1")]
    Attempt,
}

impl ProvenanceRefusal {
    /// The refusal's code, for a log line.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::OtherRun => "other-run",
            Self::OtherRepository => "other-repository",
            Self::Workflow => "workflow",
            Self::Event => "event",
            Self::Branch => "branch",
            Self::HeadRepository => "head-repository",
            Self::Attempt => "attempt",
        }
    }
}

/// Whether `facts` is a run `expected` may believe a bundle of.
///
/// The run must be the pointer's run of the pointer's repository, have executed exactly
/// `expected.workflow`'s file, have been started by an event that workflow runs for, on the
/// default branch unless the event is `pull_request_target` (whose head branch is the PR's),
/// with this repository as head repository, and both it and the pointer must be attempt 1.
/// The checks run in that order, and the first that fails is the refusal.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU64;
///
/// use assay_app::{ExpectedRun, ProvenanceRefusal, RunEvent, Workflow, WorkflowRunFacts};
/// use assay_domain::RunUid;
///
/// let id = |value| NonZeroU64::new(value).ok_or("zero");
/// let run = RunUid { repo_id: id(9)?, run_id: id(7)?, run_attempt: id(1)? };
/// let facts = WorkflowRunFacts {
///     run_id: run.run_id,
///     repo_id: run.repo_id,
///     path: ".github/workflows/assay.yml".to_owned(),
///     event: RunEvent::WorkflowDispatch,
///     head_branch: "feature".to_owned(),
///     head_repo: Some(run.repo_id),
///     run_attempt: run.run_attempt,
/// };
/// let expected = ExpectedRun { workflow: Workflow::Pipeline, run, default_branch: "main" };
///
/// assert_eq!(assay_app::accept_run(&facts, &expected), Err(ProvenanceRefusal::Branch));
/// # Ok::<(), &str>(())
/// ```
///
/// # Errors
///
/// The first [`ProvenanceRefusal`] that applies.
pub fn accept_run(
    facts: &WorkflowRunFacts,
    expected: &ExpectedRun<'_>,
) -> Result<(), ProvenanceRefusal> {
    let run = expected.run;
    if facts.run_id != run.run_id {
        return Err(ProvenanceRefusal::OtherRun);
    }
    if facts.repo_id != run.repo_id {
        return Err(ProvenanceRefusal::OtherRepository);
    }
    if facts.path != expected.workflow.path() {
        return Err(ProvenanceRefusal::Workflow);
    }
    if !expected.workflow.runs_for(facts.event) {
        return Err(ProvenanceRefusal::Event);
    }
    let on_a_pull = facts.event == RunEvent::PullRequestTarget;
    if !on_a_pull && facts.head_branch != expected.default_branch {
        return Err(ProvenanceRefusal::Branch);
    }
    if facts.head_repo != Some(run.repo_id) {
        return Err(ProvenanceRefusal::HeadRepository);
    }
    if facts.run_attempt.get() != 1 || run.run_attempt.get() != 1 {
        return Err(ProvenanceRefusal::Attempt);
    }
    Ok(())
}
