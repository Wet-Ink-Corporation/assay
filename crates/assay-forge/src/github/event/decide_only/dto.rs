//! The decide-only payloads' wire DTOs. Unknown fields are ignored, as GitHub adds fields;
//! every field read is required and checked in the conversion. A key whose absence would be
//! read as a fact (`performed_via_github_app`, through the adapter's shared reader, and
//! `issue.pull_request`) is read in three states, and the one GitHub does not document is
//! refused (wi-b5affa).

use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use super::super::dto::{EndDto, RepoDto, UserDto, head_repository};
use super::super::{branch, commit, positive, shown};
use super::{
    CommentPayload, DecideOnlyEventError, Label, LabelAction, LabelName, LabelNameError,
    LabelPayload, Sender, SenderKind,
};
use crate::github::dto::ViaApp;
use assay_app::{
    CommentAction, CommentEvent, CommentId, IssueComment, MAX_COMMENT_BYTES, Timestamp, UserId,
};

/// What `issue.pull_request` says.
#[derive(Debug, Default, PartialEq, Eq)]
enum PullLink {
    /// The key is absent (serde's `default`): the issue is not a PR.
    #[default]
    Absent,
    /// `null`, which GitHub does not document.
    Null,
    /// An object: the issue is a PR.
    Present,
}

/// A present `issue.pull_request`: `null`, or an object (its fields are not read).
fn pull_link<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PullLink, D::Error> {
    Option::<Map<String, Value>>::deserialize(deserializer).map(|link| {
        if link.is_some() {
            PullLink::Present
        } else {
            PullLink::Null
        }
    })
}

/// The event's sender.
#[derive(Debug, Deserialize)]
pub(super) struct SenderDto {
    id: u64,
    #[serde(rename = "type")]
    kind: String,
}

impl SenderDto {
    /// The sender, its type one of [`SenderKind`].
    fn into_sender(self) -> Result<Sender, DecideOnlyEventError> {
        let kind = match self.kind.as_str() {
            "User" => SenderKind::User,
            "Bot" => SenderKind::Bot,
            "Organization" => SenderKind::Organization,
            other => return Err(DecideOnlyEventError::Sender(shown(other))),
        };
        Ok(Sender {
            id: UserId(positive(self.id, "sender.id")?),
            kind,
        })
    }
}

/// The issue an `issue_comment` is on.
#[derive(Debug, Deserialize)]
pub(super) struct IssueDto {
    number: u64,
    /// Absent when the issue is not a PR.
    #[serde(default, deserialize_with = "pull_link")]
    pull_request: PullLink,
}

/// The comment in an `issue_comment` payload.
#[derive(Debug, Deserialize)]
pub(super) struct CommentDto {
    id: u64,
    /// `null` for a deleted account.
    user: Option<UserDto>,
    created_at: String,
    updated_at: String,
    body: String,
    #[serde(default, deserialize_with = "crate::github::dto::via_app")]
    performed_via_github_app: ViaApp,
}

impl CommentDto {
    /// The comment, every field checked.
    fn into_comment(self) -> Result<IssueComment, DecideOnlyEventError> {
        if self.body.len() > MAX_COMMENT_BYTES {
            return Err(DecideOnlyEventError::Field("comment.body"));
        }
        let via_app = match self.performed_via_github_app {
            ViaApp::Yes => true,
            ViaApp::No => false,
            ViaApp::Unstated => {
                return Err(DecideOnlyEventError::Field(
                    "comment.performed_via_github_app",
                ));
            }
        };
        let time = |text: &str, field| {
            text.parse::<Timestamp>()
                .map_err(|_refused| DecideOnlyEventError::Field(field))
        };
        let user = self
            .user
            .map(|user| positive(user.id, "comment.user.id").map(UserId))
            .transpose()?;
        Ok(IssueComment {
            id: CommentId(positive(self.id, "comment.id")?),
            user,
            created_at: time(&self.created_at, "comment.created_at")?,
            updated_at: time(&self.updated_at, "comment.updated_at")?,
            body: self.body,
            via_app,
        })
    }
}

/// An `issue_comment` payload.
#[derive(Debug, Deserialize)]
pub(super) struct CommentEventDto {
    action: String,
    issue: IssueDto,
    comment: CommentDto,
    repository: RepoDto,
    sender: SenderDto,
}

impl CommentEventDto {
    /// The payload, every field checked.
    pub(super) fn into_payload(self) -> Result<CommentPayload, DecideOnlyEventError> {
        let action = match self.action.as_str() {
            "created" => CommentAction::Created,
            "edited" => CommentAction::Edited,
            "deleted" => CommentAction::Deleted,
            other => return Err(DecideOnlyEventError::Action(shown(other))),
        };
        let number = positive(self.issue.number, "issue.number")?;
        let pr = match self.issue.pull_request {
            PullLink::Absent => None,
            PullLink::Present => Some(number),
            PullLink::Null => return Err(DecideOnlyEventError::Field("issue.pull_request")),
        };
        let sender = self.sender.into_sender()?;
        let comment = self.comment.into_comment()?;
        // A comment is created by its author, so the sender the guard judges is the author the
        // recording rule reads. Another user may edit or delete it, so only `created` is held.
        if action == CommentAction::Created
            && comment.user.is_some_and(|author| author != sender.id)
        {
            return Err(DecideOnlyEventError::Field("sender.id"));
        }
        Ok(CommentPayload {
            repository_id: positive(self.repository.id, "repository.id")?,
            pr,
            sender,
            event: CommentEvent { action, comment },
        })
    }
}

/// A label event's PR; only what a label event needs is read.
#[derive(Debug, Deserialize)]
pub(super) struct LabelPullDto {
    number: u64,
    head: EndDto,
    base: EndDto,
}

/// The label a label event names.
#[derive(Debug, Deserialize)]
pub(super) struct LabelDto {
    name: String,
}

/// A `pull_request_target` `labeled` or `unlabeled` payload.
#[derive(Debug, Deserialize)]
pub(super) struct LabelEventDto {
    action: String,
    number: u64,
    pull_request: LabelPullDto,
    label: Option<LabelDto>,
    repository: RepoDto,
    sender: SenderDto,
}

impl LabelEventDto {
    /// The payload, every field checked; a label name over the cap is reported, not refused.
    pub(super) fn into_payload(self) -> Result<LabelPayload, DecideOnlyEventError> {
        let action = match self.action.as_str() {
            "labeled" => LabelAction::Labeled,
            "unlabeled" => LabelAction::Unlabeled,
            other => return Err(DecideOnlyEventError::Action(shown(other))),
        };
        let Self {
            number,
            pull_request: pull,
            label,
            repository,
            sender,
            ..
        } = self;
        let number = positive(number, "number")?;
        if pull.number != number.get() {
            return Err(DecideOnlyEventError::Field("number"));
        }
        let repository_id = positive(repository.id, "repository.id")?;
        let label = label.ok_or(DecideOnlyEventError::Field("label"))?;
        Ok(LabelPayload {
            action,
            label: match LabelName::try_from(label.name) {
                Ok(name) => Label::Name(name),
                Err(LabelNameError::OverCap { bytes }) => Label::OverCap { bytes },
                Err(LabelNameError::Empty | LabelNameError::Control) => {
                    return Err(DecideOnlyEventError::Field("label.name"));
                }
            },
            number,
            head_sha: commit(&pull.head.sha, "pull_request.head.sha")?,
            head_repository: head_repository(pull.head.repo, pull.base.repo, repository_id)?,
            base_ref: branch(pull.base.git_ref, "pull_request.base.ref")?,
            sender: sender.into_sender()?,
            repository_id,
        })
    }
}

/// A `schedule` payload: the cron that fired and the repository.
#[derive(Debug, Deserialize)]
pub(super) struct ScheduleDto {
    schedule: String,
    repository: RepoDto,
}

impl ScheduleDto {
    /// The repository the poll is for; the cron must be present and not empty.
    pub(super) fn into_repository_id(self) -> Result<NonZeroU64, DecideOnlyEventError> {
        if self.schedule.trim().is_empty() {
            return Err(DecideOnlyEventError::Field("schedule"));
        }
        Ok(positive(self.repository.id, "repository.id")?)
    }
}
