//! The per-repo reconcile's port (06 §4) over the adapter's calls: the open PRs, the App's
//! checks on a head, the workflow run a verdict check's stamp names, and the compare-and-set
//! check update `decide` uses.

use std::num::NonZeroU64;

use assay_app::{
    CheckRunId, CheckUpdate, CommitId, CompareAndSetError, LedgerTime, OpenPull, PullHead,
    ReconcileCheck, ReconcileForge, RunProgress, StampedRun,
};
use serde::Deserialize;

use super::GitHub;
use super::api::PER_PAGE;
use super::checks::check_status;
use super::dto::{self, CheckRunDto, PullDto};
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};

/// A workflow run as the reconcile reads it.
#[derive(Debug, Deserialize)]
struct StampedRunDto {
    id: u64,
    status: String,
    run_started_at: Option<String>,
}

impl<T: Transport, S: Sleep> ReconcileForge for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn open_pulls(&self, limit: usize) -> Result<Vec<OpenPull>, Self::Error> {
        let operation = Operation::ListPulls;
        let path = self.path(operation, &format!("pulls?state=open&per_page={PER_PAGE}"))?;
        let mut pulls = self
            .api
            .list::<Vec<PullDto>>(operation, path, Some(limit))?
            .into_iter()
            .map(|pull| {
                if pull.state != "open" {
                    return Err(ForgeError::Malformed {
                        operation,
                        detail: "a listed open PR is not open",
                    });
                }
                Ok(OpenPull {
                    number: dto::nonzero(operation, pull.number, "a PR number is zero")?,
                    head_sha: dto::commit(operation, &pull.head.sha)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        pulls.truncate(limit);
        Ok(pulls)
    }

    /// Reads every check on `head` before keeping the App's, so that a list cut at `limit` is
    /// never cut before the filter.
    fn app_checks(
        &self,
        head: &CommitId,
        limit: usize,
    ) -> Result<Vec<ReconcileCheck>, Self::Error> {
        let operation = Operation::ListChecks;
        let mut checks = Vec::new();
        for check in self.list_checks(head, None, None)? {
            if dto::app(operation, check.app.as_ref())? != self.identity.app {
                continue;
            }
            if check.head_sha != head.as_str() {
                return Err(ForgeError::Malformed {
                    operation,
                    detail: "a listed check is on another head",
                });
            }
            checks.push(reconcile_check(operation, check)?);
        }
        checks.truncate(limit);
        Ok(checks)
    }

    /// A 404 is a run GitHub does not have: the sweep has just listed this repository's PRs
    /// with the same token, so the token sees the repository.
    fn workflow_run(&self, run_id: NonZeroU64) -> Result<Option<StampedRun>, Self::Error> {
        let operation = Operation::ListRuns;
        let path = self.path(operation, &format!("actions/runs/{run_id}"))?;
        let Some(run) = self.api.get_optional::<StampedRunDto>(operation, &path)? else {
            return Ok(None);
        };
        if run.id != run_id.get() {
            return Err(ForgeError::Malformed {
                operation,
                detail: "GitHub returned another run",
            });
        }
        let progress = match run.status.as_str() {
            "completed" => RunProgress::Completed,
            "queued" | "in_progress" | "waiting" | "pending" | "requested" => RunProgress::Live,
            _ => {
                return Err(ForgeError::Malformed {
                    operation,
                    detail: "a run's status is not one GitHub documents",
                });
            }
        };
        let started_at = run.run_started_at.as_deref().ok_or(ForgeError::Malformed {
            operation,
            detail: "a run has no run_started_at",
        })?;
        Ok(Some(StampedRun {
            progress,
            started_at: github_time(operation, started_at)?,
        }))
    }

    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, Self::Error> {
        self.list_labels(number)
    }

    fn pull_head(&self, number: NonZeroU64) -> Result<PullHead, Self::Error> {
        let operation = Operation::ReadPull;
        let path = self.path(operation, &format!("pulls/{number}"))?;
        let pull: PullDto = self.api.get(operation, &path)?;
        if pull.number != number.get() {
            return Err(ForgeError::Malformed {
                operation,
                detail: "GitHub returned another PR",
            });
        }
        match pull.state.as_str() {
            "open" => Ok(PullHead::Open(dto::commit(operation, &pull.head.sha)?)),
            "closed" => Ok(PullHead::Closed),
            _ => Err(ForgeError::Malformed {
                operation,
                detail: "a PR is neither open nor closed",
            }),
        }
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        update: &CheckUpdate,
    ) -> Result<(), CompareAndSetError<Self::Error>> {
        self.compare_and_set(id, expected, update)
    }
}

/// The App's `check` as the reconcile reads it.
fn reconcile_check<E>(
    operation: Operation,
    check: CheckRunDto,
) -> Result<ReconcileCheck, ForgeError<E>> {
    Ok(ReconcileCheck {
        id: CheckRunId(dto::nonzero(operation, check.id, "a check id is zero")?),
        external_id: check
            .external_id
            .filter(|external_id| !external_id.is_empty()),
        status: check_status(operation, &check.status)?,
        name: check.name,
    })
}

/// GitHub's `YYYY-MM-DDTHH:MM:SSZ` as a ledger time; any other form is refused.
fn github_time<E>(operation: Operation, text: &str) -> Result<LedgerTime, ForgeError<E>> {
    let refused = || ForgeError::Malformed {
        operation,
        detail: "a time is not YYYY-MM-DDTHH:MM:SSZ",
    };
    let seconds = text.strip_suffix('Z').ok_or_else(refused)?;
    if seconds.contains('.') {
        return Err(refused());
    }
    format!("{seconds}.000Z")
        .parse()
        .map_err(|_refused| refused())
}
