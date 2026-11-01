//! The event payloads' wire DTOs. Unknown fields are ignored, as GitHub adds fields; every
//! field read is required and checked in the conversion, except the dispatch's `inputs`,
//! which is a closed set (R3-14).

use std::num::NonZeroU64;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::{
    DispatchPayload, EventPayloadError, HeadRepository, InvalidField, PullRequestAction,
    PullRequestPayload, branch, canonical_positive, commit, positive, shown,
};
use assay_app::{CommitId, UserId};

/// A user as the payload embeds one.
#[derive(Debug, Deserialize)]
pub(super) struct UserDto {
    pub(super) id: u64,
}

/// A repository as the payload embeds one.
#[derive(Debug, Deserialize)]
pub(super) struct RepoDto {
    pub(super) id: u64,
}

/// The repository the event is for.
#[derive(Debug, Deserialize)]
pub(super) struct RepositoryDto {
    id: u64,
    default_branch: String,
}

/// One side of a PR: `head` or `base`.
#[derive(Debug, Deserialize)]
pub(super) struct EndDto {
    #[serde(rename = "ref")]
    pub(super) git_ref: String,
    pub(super) sha: String,
    /// `null` for a head whose repository was deleted.
    pub(super) repo: Option<RepoDto>,
}

/// Where a PR's head lives, for a PR whose base repository (`base`) must be the event's
/// (`repository_id`). A head GitHub reports as `null` was deleted; reported, never dropped.
pub(super) fn head_repository(
    head: Option<RepoDto>,
    base: Option<RepoDto>,
    repository_id: NonZeroU64,
) -> Result<HeadRepository, InvalidField> {
    let base = base.ok_or(InvalidField("pull_request.base.repo"))?;
    if base.id != repository_id.get() {
        return Err(InvalidField("pull_request.base.repo.id"));
    }
    Ok(match head {
        None => HeadRepository::Deleted,
        Some(repo) if repo.id == repository_id.get() => HeadRepository::Base,
        Some(repo) => HeadRepository::Other(positive(repo.id, "pull_request.head.repo.id")?),
    })
}

/// The PR.
#[derive(Debug, Deserialize)]
pub(super) struct PullDto {
    number: u64,
    draft: bool,
    user: Option<UserDto>,
    head: EndDto,
    base: EndDto,
}

/// An `edited` event's changes; only what `base` says is read.
#[derive(Debug, Deserialize)]
pub(super) struct ChangesDto {
    #[serde(default, deserialize_with = "base_change")]
    base: BaseChange,
}

/// What `changes.base` says. Only an object is a base change; a present `null` is refused
/// rather than read as no change (wi-b5affa).
#[derive(Debug, Default, PartialEq, Eq)]
enum BaseChange {
    /// The key is absent (serde's `default`): the edit did not change the base.
    #[default]
    Absent,
    /// `null`, which GitHub does not document.
    Null,
    /// An object: the base changed.
    Changed,
}

/// A present `changes.base`: `null`, or an object (its fields are not read).
fn base_change<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<BaseChange, D::Error> {
    Option::<Map<String, Value>>::deserialize(deserializer).map(|base| {
        if base.is_some() {
            BaseChange::Changed
        } else {
            BaseChange::Null
        }
    })
}

/// A `pull_request_target` payload.
#[derive(Debug, Deserialize)]
pub(super) struct PullRequestEventDto {
    action: String,
    number: u64,
    pull_request: PullDto,
    repository: RepositoryDto,
    sender: UserDto,
    before: Option<String>,
    after: Option<String>,
    changes: Option<ChangesDto>,
}

impl PullRequestEventDto {
    /// The action, for a PR whose head is `head_sha`: a `synchronize` needs `before`, and its
    /// `after` must be that head.
    fn action(&self, head_sha: &CommitId) -> Result<PullRequestAction, EventPayloadError> {
        Ok(match self.action.as_str() {
            "opened" => PullRequestAction::Opened,
            "synchronize" => {
                let before = self
                    .before
                    .as_deref()
                    .ok_or(EventPayloadError::Field("before"))?;
                if self.after.as_deref() != Some(head_sha.as_str()) {
                    return Err(EventPayloadError::Field("after"));
                }
                PullRequestAction::Synchronize {
                    before: commit(before, "before")?,
                }
            }
            "reopened" => PullRequestAction::Reopened,
            "ready_for_review" => PullRequestAction::ReadyForReview,
            "edited" => {
                let changes = self
                    .changes
                    .as_ref()
                    .ok_or(EventPayloadError::Field("changes"))?;
                PullRequestAction::Edited {
                    base_changed: match changes.base {
                        BaseChange::Absent => false,
                        BaseChange::Changed => true,
                        BaseChange::Null => return Err(EventPayloadError::Field("changes.base")),
                    },
                }
            }
            other => return Err(EventPayloadError::Action(shown(other))),
        })
    }

    /// The payload, every field checked.
    pub(super) fn into_payload(self) -> Result<PullRequestPayload, EventPayloadError> {
        let head_sha = commit(&self.pull_request.head.sha, "pull_request.head.sha")?;
        let action = self.action(&head_sha)?;
        let Self {
            number,
            pull_request: pull,
            repository,
            sender,
            ..
        } = self;
        let number = positive(number, "number")?;
        if pull.number != number.get() {
            return Err(EventPayloadError::Field("number"));
        }
        let repository_id = positive(repository.id, "repository.id")?;
        let head_repository = head_repository(pull.head.repo, pull.base.repo, repository_id)?;
        let author = pull
            .user
            .ok_or(EventPayloadError::Field("pull_request.user"))?;
        Ok(PullRequestPayload {
            action,
            number,
            head_sha,
            base_sha: commit(&pull.base.sha, "pull_request.base.sha")?,
            base_ref: branch(pull.base.git_ref, "pull_request.base.ref")?,
            draft: pull.draft,
            author: UserId(positive(author.id, "pull_request.user.id")?),
            sender: UserId(positive(sender.id, "sender.id")?),
            repository_id,
            head_repository,
            default_branch: branch(repository.default_branch, "repository.default_branch")?,
        })
    }
}

/// A `workflow_dispatch` payload.
#[derive(Debug, Deserialize)]
pub(super) struct DispatchDto {
    inputs: Option<Map<String, Value>>,
    repository: RepositoryDto,
    sender: UserDto,
}

impl DispatchDto {
    /// The payload, every field checked and no input but `pr`.
    pub(super) fn into_payload(self) -> Result<DispatchPayload, EventPayloadError> {
        let Some(inputs) = self.inputs else {
            return Err(EventPayloadError::Field("inputs.pr"));
        };
        if let Some(name) = inputs.keys().find(|name| name.as_str() != "pr") {
            return Err(EventPayloadError::DispatchInput(shown(name)));
        }
        let pr = inputs
            .get("pr")
            .and_then(Value::as_str)
            .and_then(canonical_positive)
            .ok_or(EventPayloadError::Field("inputs.pr"))?;
        Ok(DispatchPayload {
            pr,
            sender: UserId(positive(self.sender.id, "sender.id")?),
            repository_id: positive(self.repository.id, "repository.id")?,
            default_branch: branch(self.repository.default_branch, "repository.default_branch")?,
        })
    }
}
