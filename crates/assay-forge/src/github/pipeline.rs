//! The reads the pipeline's jobs make besides the app's ports (#60): a PR's identity for a
//! `workflow_dispatch` run, the workflow run's `created_at` (the push clock), whether a push
//! rewrote history, and the PR's REST file list (`files.rs`). Each refuses an answer about
//! another object, and anything it cannot read exactly (wi-b5affa).

mod files;

use std::num::NonZeroU64;

use assay_app::{CommitId, LedgerTime, UserId};
use serde::Deserialize;

use super::GitHub;
use super::dto::{self, UserDto};
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};

/// A PR as `assay guard` identifies it for a `workflow_dispatch` run, which carries only its
/// number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullIdentity {
    /// The PR number.
    pub number: NonZeroU64,
    /// The head commit.
    pub head_sha: CommitId,
    /// The base commit.
    pub base_sha: CommitId,
    /// The base branch.
    pub base_ref: String,
    /// Whether the PR is a draft.
    pub draft: bool,
    /// The PR's author.
    pub author: UserId,
    /// The id of the repository the head is in; `None` once that fork is deleted.
    pub head_repo: Option<NonZeroU64>,
    /// Whether the PR is open.
    pub state: PullState,
}

/// Whether a PR is open. A merged PR is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullState {
    /// `open`.
    Open,
    /// `closed`, merged or not.
    Closed,
}

/// A PR as [`GitHub::pull_identity`] reads it.
#[derive(Debug, Deserialize)]
struct IdentityDto {
    number: u64,
    state: String,
    draft: bool,
    user: Option<UserDto>,
    head: IdentityHeadDto,
    base: IdentityBaseDto,
}

/// A PR's head, with the repository's id.
#[derive(Debug, Deserialize)]
struct IdentityHeadDto {
    sha: String,
    repo: Option<RepoIdDto>,
}

/// A PR's base commit and branch.
#[derive(Debug, Deserialize)]
struct IdentityBaseDto {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
}

/// A repository by id.
#[derive(Debug, Deserialize)]
struct RepoIdDto {
    id: u64,
}

/// A workflow run's id and creation time.
#[derive(Debug, Deserialize)]
struct CreatedRunDto {
    id: u64,
    created_at: String,
}

/// `compare/<base>...<head>`, without its commit and file lists.
#[derive(Debug, Deserialize)]
struct CompareDto {
    status: String,
    behind_by: u64,
    base_commit: ShaDto,
    merge_base_commit: ShaDto,
}

/// A commit by id.
#[derive(Debug, Deserialize)]
struct ShaDto {
    sha: String,
}

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// PR `number`: its commits, base branch, draft flag, author, head repository and state.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Malformed`] when GitHub answers for another PR, names no author, a zero
    /// id, a short commit id, an empty base branch or a state other than `open` and `closed`;
    /// the read's errors.
    pub fn pull_identity(&self, number: NonZeroU64) -> Result<PullIdentity, ForgeError<T::Error>> {
        let operation = Operation::ReadPull;
        let path = self.path(operation, &format!("pulls/{number}"))?;
        let pull: IdentityDto = self.api.get(operation, &path)?;
        same_pull(operation, number, pull.number)?;
        let state = match pull.state.as_str() {
            "open" => PullState::Open,
            "closed" => PullState::Closed,
            _ => {
                return Err(ForgeError::Malformed {
                    operation,
                    detail: "a PR is neither open nor closed",
                });
            }
        };
        let author = dto::user(operation, pull.user.as_ref())?.ok_or(ForgeError::Malformed {
            operation,
            detail: "a PR has no author",
        })?;
        let head_repo = pull
            .head
            .repo
            .map(|repo| dto::nonzero(operation, repo.id, "a repository id is zero"))
            .transpose()?;
        if pull.base.branch.is_empty() {
            return Err(ForgeError::Malformed {
                operation,
                detail: "a PR's base branch is empty",
            });
        }
        Ok(PullIdentity {
            number,
            head_sha: dto::commit(operation, &pull.head.sha)?,
            base_sha: dto::commit(operation, &pull.base.sha)?,
            base_ref: pull.base.branch,
            draft: pull.draft,
            author,
            head_repo,
            state,
        })
    }

    /// When workflow run `run_id` was created: a push record's `at` (04 §5).
    ///
    /// # Errors
    ///
    /// [`ForgeError::Malformed`] when GitHub answers for another run, or `created_at` is not
    /// GitHub's `YYYY-MM-DDTHH:MM:SSZ`; the read's errors.
    pub fn run_created_at(&self, run_id: NonZeroU64) -> Result<LedgerTime, ForgeError<T::Error>> {
        let operation = Operation::ListRuns;
        let path = self.path(operation, &format!("actions/runs/{run_id}"))?;
        let run: CreatedRunDto = self.api.get(operation, &path)?;
        if run.id != run_id.get() {
            return Err(ForgeError::Malformed {
                operation,
                detail: "GitHub returned another run",
            });
        }
        dto::github_time(operation, &run.created_at)
    }

    /// Whether moving a branch from `before` to `after` rewrote history: `after` does not
    /// contain `before` (compare status `behind` or `diverged`). `ahead` and `identical` are a
    /// fast-forward, believed only when the comparison says `before` is the merge base.
    ///
    /// The comparison asks for one commit per page, so its commit list does not grow the
    /// body; GitHub still lists up to 300 files, and a body over the response cap is refused.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Malformed`] when GitHub compared another base, a status is not one it
    /// documents, or a fast-forward is behind its base or has another merge base; the read's
    /// errors.
    pub fn forced(
        &self,
        before: &CommitId,
        after: &CommitId,
    ) -> Result<bool, ForgeError<T::Error>> {
        let operation = Operation::CompareCommits;
        let path = self.path(operation, &format!("compare/{before}...{after}?per_page=1"))?;
        let compare: CompareDto = self.api.get(operation, &path)?;
        let malformed = |detail| ForgeError::Malformed { operation, detail };
        if compare.base_commit.sha != before.as_str() {
            return Err(malformed("GitHub compared another base"));
        }
        match compare.status.as_str() {
            "behind" | "diverged" => Ok(true),
            "ahead" | "identical" => {
                if compare.behind_by != 0 {
                    return Err(malformed("a fast-forward comparison is behind its base"));
                }
                if compare.merge_base_commit.sha != before.as_str() {
                    return Err(malformed(
                        "a fast-forward comparison's merge base is not its base",
                    ));
                }
                Ok(false)
            }
            _ => Err(malformed(
                "a comparison's status is not one GitHub documents",
            )),
        }
    }
}

/// Refuses an answer GitHub gave for PR `got` when PR `number` was asked for.
fn same_pull<E>(operation: Operation, number: NonZeroU64, got: u64) -> Result<(), ForgeError<E>> {
    if got == number.get() {
        Ok(())
    } else {
        Err(ForgeError::Malformed {
            operation,
            detail: "GitHub returned another PR",
        })
    }
}
