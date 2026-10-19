//! `decide`'s view of the PR now: its head and base, the PRs on its head, and whether a newer
//! run of the same workflow exists for the head (02 §5).

use std::num::NonZeroU64;

use assay_app::{CommitId, CurrentPullRequest, PullOnHead, PullRequestState};
use assay_domain::RunUid;

use super::GitHub;
use super::api::{PER_PAGE, Page};
use super::dto::{self, PullDto, RunDto, RunsPage};
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};

impl Page for RunsPage {
    type Item = RunDto;

    fn items(self) -> Vec<RunDto> {
        self.workflow_runs
    }
}

impl<T: Transport, S: Sleep> PullRequestState for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn current(&self, number: NonZeroU64) -> Result<CurrentPullRequest, Self::Error> {
        let operation = Operation::ReadPull;
        let pull: PullDto = self
            .api
            .get(operation, &self.path(&format!("pulls/{number}")))?;
        Ok(CurrentPullRequest {
            head_sha: dto::commit(operation, &pull.head.sha)?,
            base_ref: pull.base.branch,
        })
    }

    fn pulls_on_head(&self, head: &CommitId) -> Result<Vec<PullOnHead>, Self::Error> {
        let operation = Operation::ListPulls;
        let path = self.path(&format!("commits/{head}/pulls?per_page={PER_PAGE}"));
        self.api
            .list::<Vec<PullDto>>(operation, &path, None)?
            .into_iter()
            .map(|pull| {
                let open = match pull.state.as_str() {
                    "open" => true,
                    "closed" => false,
                    _ => {
                        return Err(ForgeError::Malformed {
                            operation,
                            detail: "a PR is neither open nor closed",
                        });
                    }
                };
                Ok(PullOnHead {
                    number: dto::nonzero(operation, pull.number, "a PR number is zero")?,
                    open,
                    base_ref: pull.base.branch,
                })
            })
            .collect()
    }

    /// Reads `run` for its workflow and run number, then lists that workflow's runs on `head`:
    /// a newer run has a higher run number.
    fn newer_run(&self, run: RunUid, head: &CommitId) -> Result<bool, Self::Error> {
        let operation = Operation::ListRuns;
        let this: RunDto = self.api.get(
            operation,
            &self.path(&format!("actions/runs/{}", run.run_id)),
        )?;
        if this.id != run.run_id.get() {
            return Err(ForgeError::Malformed {
                operation,
                detail: "GitHub returned another run",
            });
        }
        let path = self.path(&format!(
            "actions/workflows/{}/runs?head_sha={head}&per_page={PER_PAGE}",
            this.workflow_id
        ));
        let runs = self.api.list::<RunsPage>(operation, &path, None)?;
        if runs.iter().any(|listed| listed.head_sha != head.as_str()) {
            return Err(ForgeError::Malformed {
                operation,
                detail: "a listed run is on another head",
            });
        }
        Ok(runs
            .iter()
            .any(|listed| listed.run_number > this.run_number))
    }
}
