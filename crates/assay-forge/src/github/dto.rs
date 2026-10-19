//! Wire DTOs for the REST responses the adapter reads (OUT-01). Unknown fields are ignored,
//! as GitHub adds fields; a missing required field, a wrong type or a repeated key is refused
//! by the decode.

use std::num::NonZeroU64;

use assay_app::{AppId, CommitId, ParseCommitIdError, UserId};
use serde::Deserialize;

use super::error::{ForgeError, Operation};

/// An app as a check run or comment names it.
#[derive(Debug, Deserialize)]
pub(crate) struct AppDto {
    pub(crate) id: u64,
}

/// A user as GitHub embeds one.
#[derive(Debug, Deserialize)]
pub(crate) struct UserDto {
    pub(crate) id: u64,
    #[serde(rename = "type")]
    pub(crate) kind: String,
}

/// A check run.
#[derive(Debug, Deserialize)]
pub(crate) struct CheckRunDto {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) head_sha: String,
    pub(crate) external_id: Option<String>,
    pub(crate) status: String,
    pub(crate) conclusion: Option<String>,
    pub(crate) output: OutputDto,
    pub(crate) app: Option<AppDto>,
    pub(crate) html_url: Option<String>,
}

/// A check run's output.
#[derive(Debug, Deserialize)]
pub(crate) struct OutputDto {
    pub(crate) title: Option<String>,
    pub(crate) text: Option<String>,
}

/// A page of `GET /commits/{sha}/check-runs`.
#[derive(Debug, Deserialize)]
pub(crate) struct CheckRunsPage {
    pub(crate) check_runs: Vec<CheckRunDto>,
}

/// An issue comment.
#[derive(Debug, Deserialize)]
pub(crate) struct IssueCommentDto {
    pub(crate) id: u64,
    pub(crate) body: Option<String>,
    pub(crate) user: Option<UserDto>,
    pub(crate) performed_via_github_app: Option<AppDto>,
}

/// A review comment on a PR's diff.
#[derive(Debug, Deserialize)]
pub(crate) struct ReviewCommentDto {
    pub(crate) body: String,
    pub(crate) path: String,
    pub(crate) line: Option<u64>,
    pub(crate) html_url: String,
    pub(crate) user: Option<UserDto>,
}

/// A PR review.
#[derive(Debug, Deserialize)]
pub(crate) struct ReviewDto {
    pub(crate) id: u64,
    pub(crate) user: Option<UserDto>,
    pub(crate) state: String,
    pub(crate) commit_id: Option<String>,
}

/// A label.
#[derive(Debug, Deserialize)]
pub(crate) struct LabelDto {
    pub(crate) name: String,
}

/// A reaction.
#[derive(Debug, Deserialize)]
pub(crate) struct ReactionDto {
    pub(crate) id: u64,
    pub(crate) user: Option<UserDto>,
    pub(crate) content: String,
}

/// A timeline event; only the fields of the kinds assay reads.
#[derive(Debug, Deserialize)]
pub(crate) struct TimelineDto {
    pub(crate) event: String,
    pub(crate) id: Option<u64>,
    pub(crate) actor: Option<UserDto>,
    pub(crate) label: Option<LabelDto>,
    pub(crate) dismissed_review: Option<DismissedReviewDto>,
}

/// What a `review_dismissed` event dismissed.
#[derive(Debug, Deserialize)]
pub(crate) struct DismissedReviewDto {
    pub(crate) review_id: u64,
}

/// A PR.
#[derive(Debug, Deserialize)]
pub(crate) struct PullDto {
    pub(crate) number: u64,
    pub(crate) state: String,
    pub(crate) head: HeadDto,
    pub(crate) base: BaseDto,
}

/// A PR's head.
#[derive(Debug, Deserialize)]
pub(crate) struct HeadDto {
    pub(crate) sha: String,
    #[serde(rename = "ref")]
    pub(crate) branch: String,
    pub(crate) repo: Option<RepoDto>,
}

/// A PR's base.
#[derive(Debug, Deserialize)]
pub(crate) struct BaseDto {
    #[serde(rename = "ref")]
    pub(crate) branch: String,
}

/// A repository as a PR's head names it; `None` once the fork is deleted.
#[derive(Debug, Deserialize)]
pub(crate) struct RepoDto {
    pub(crate) full_name: String,
}

/// The repository itself (`GET repos/<owner>/<name>`), as `assay status` reads it.
#[derive(Debug, Deserialize)]
pub(crate) struct RepositoryDto {
    pub(crate) id: u64,
    pub(crate) full_name: String,
    pub(crate) default_branch: String,
}

/// The authenticated user.
#[derive(Debug, Deserialize)]
pub(crate) struct ViewerDto {
    pub(crate) id: u64,
    pub(crate) login: String,
}

/// A workflow run.
#[derive(Debug, Deserialize)]
pub(crate) struct RunDto {
    pub(crate) id: u64,
    pub(crate) workflow_id: u64,
    pub(crate) run_number: u64,
    pub(crate) head_sha: String,
}

/// A page of `GET /actions/workflows/{id}/runs`.
#[derive(Debug, Deserialize)]
pub(crate) struct RunsPage {
    pub(crate) workflow_runs: Vec<RunDto>,
}

/// `id` as a non-zero id, or a refusal naming `what`.
pub(crate) fn nonzero<E>(
    operation: Operation,
    id: u64,
    what: &'static str,
) -> Result<NonZeroU64, ForgeError<E>> {
    NonZeroU64::new(id).ok_or(ForgeError::Malformed {
        operation,
        detail: what,
    })
}

/// `sha` as a full commit id, or a refusal.
pub(crate) fn commit<E>(operation: Operation, sha: &str) -> Result<CommitId, ForgeError<E>> {
    sha.parse()
        .map_err(|ParseCommitIdError| ForgeError::Malformed {
            operation,
            detail: "a commit id is not 40 lowercase hex digits",
        })
}

/// The user's id, or `None` for a deleted account (GitHub sends `null`).
pub(crate) fn user<E>(
    operation: Operation,
    user: Option<&UserDto>,
) -> Result<Option<UserId>, ForgeError<E>> {
    user.map(|user| nonzero(operation, user.id, "a user id is zero").map(UserId))
        .transpose()
}

/// The app's id, or a refusal when GitHub names none.
pub(crate) fn app<E>(operation: Operation, app: Option<&AppDto>) -> Result<AppId, ForgeError<E>> {
    let app = app.ok_or(ForgeError::Malformed {
        operation,
        detail: "a check run names no app",
    })?;
    nonzero(operation, app.id, "an app id is zero").map(AppId)
}
