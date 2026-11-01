//! `assay-app`'s [`RunProvenance`] over `GET actions/runs/{id}` (J4): the facts provenance
//! judges a workflow run by, and nothing else of the answer.
//!
//! GitHub's answer is read strictly where provenance depends on it: another run's answer is
//! refused, and so are a zero id and a missing head branch. An event assay never runs for is
//! [`RunEvent::Other`], carrying nothing GitHub said; a missing head repository is `None`. The
//! app decides what is believed (wi-b5affa).

use std::num::NonZeroU64;

use assay_app::{RunEvent, RunProvenance, WorkflowRunFacts};
use serde::Deserialize;

use super::GitHub;
use super::dto;
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};

/// A workflow run as provenance reads it.
#[derive(Debug, Deserialize)]
struct ProvenanceRunDto {
    id: u64,
    path: String,
    event: String,
    head_branch: Option<String>,
    run_attempt: u64,
    repository: RunRepositoryDto,
    head_repository: Option<RunRepositoryDto>,
}

/// A repository a run names.
#[derive(Debug, Deserialize)]
struct RunRepositoryDto {
    id: u64,
}

impl<T: Transport, S: Sleep> RunProvenance for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    /// A 404 is a run GitHub does not have: the token reads this repository's runs (decide-only
    /// holds `actions: read`), so a pointer to it names no run.
    fn workflow_run(&self, run_id: NonZeroU64) -> Result<Option<WorkflowRunFacts>, Self::Error> {
        let operation = Operation::ListRuns;
        let path = self.path(operation, &format!("actions/runs/{run_id}"))?;
        let Some(run) = self
            .api
            .get_optional::<ProvenanceRunDto>(operation, &path)?
        else {
            return Ok(None);
        };
        let malformed = |detail| ForgeError::Malformed { operation, detail };
        if run.id != run_id.get() {
            return Err(malformed("GitHub returned another run"));
        }
        let head_branch = run
            .head_branch
            .ok_or_else(|| malformed("a run has no head branch"))?;
        let head_repo = run
            .head_repository
            .map(|repo| dto::nonzero(operation, repo.id, "a repository id is zero"))
            .transpose()?;
        Ok(Some(WorkflowRunFacts {
            run_id,
            repo_id: dto::nonzero(operation, run.repository.id, "a repository id is zero")?,
            path: run.path,
            event: event(&run.event),
            head_branch,
            head_repo,
            run_attempt: dto::nonzero(operation, run.run_attempt, "a run attempt is zero")?,
        }))
    }
}

/// The event GitHub names, as provenance judges it.
fn event(name: &str) -> RunEvent {
    match name {
        "pull_request_target" => RunEvent::PullRequestTarget,
        "workflow_dispatch" => RunEvent::WorkflowDispatch,
        "issue_comment" => RunEvent::IssueComment,
        "schedule" => RunEvent::Schedule,
        _ => RunEvent::Other,
    }
}
