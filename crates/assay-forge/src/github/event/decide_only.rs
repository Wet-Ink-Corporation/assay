//! The payload of the event behind a decide-only run (#61): an `issue_comment`, a
//! `pull_request_target` `labeled` or `unlabeled`, or the hourly `schedule`. Read like the
//! pipeline's payload: a DTO, then a checked conversion into closed types (OUT-01); anything
//! outside that shape is refused, never defaulted (wi-b5affa). What a run does with the event
//! (a command, a label of assay's, the poll) is the guard's to decide, so a fork head, a deleted
//! head and an over-long label name are reported here, never dropped.

mod dto;

use std::io::Read;
use std::num::NonZeroU64;
use std::str::FromStr;

use assay_app::{CommentEvent, CommitId, UserId};

use self::dto::{CommentEventDto, LabelEventDto, ScheduleDto};
use super::{HeadRepository, InvalidField, MAX_EVENT_BYTES, Unread, read_json};

/// The longest label name kept, in bytes; a longer one is [`Label::OverCap`].
pub const MAX_LABEL_BYTES: usize = 64;

/// The events a decide-only run is triggered by, as `GITHUB_EVENT_NAME` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecideOnlyEventName {
    /// `issue_comment`: a comment on an issue or a PR.
    IssueComment,
    /// `pull_request_target`: a label added to or removed from a PR.
    PullRequestTarget,
    /// `schedule`: the hourly poll.
    Schedule,
}

/// `GITHUB_EVENT_NAME` names an event decide-only does not run for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("decide-only runs only for `issue_comment`, `pull_request_target` and `schedule`")]
pub struct ParseDecideOnlyEventNameError;

impl FromStr for DecideOnlyEventName {
    type Err = ParseDecideOnlyEventNameError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "issue_comment" => Ok(Self::IssueComment),
            "pull_request_target" => Ok(Self::PullRequestTarget),
            "schedule" => Ok(Self::Schedule),
            _ => Err(ParseDecideOnlyEventNameError),
        }
    }
}

/// What kind of account `sender` is (`sender.type`); any other type is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SenderKind {
    /// `User`.
    User,
    /// `Bot`: an App's or an integration's `[bot]` user.
    Bot,
    /// `Organization`.
    Organization,
}

/// The event's sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sender {
    /// `sender.id`.
    pub id: UserId,
    /// `sender.type`.
    pub kind: SenderKind,
}

/// An `issue_comment` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentPayload {
    /// The repository the event is for (`repository.id`).
    pub repository_id: NonZeroU64,
    /// The PR commented on (`issue.number`); `None` when the issue is not a PR (the payload's
    /// `issue` has no `pull_request`).
    pub pr: Option<NonZeroU64>,
    /// The event's sender.
    pub sender: Sender,
    /// The action and the comment in the payload, as `assay-app`'s recording rule reads them.
    pub event: CommentEvent,
}

/// A `pull_request_target` label action decide-only runs for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelAction {
    /// `labeled`.
    Labeled,
    /// `unlabeled`.
    Unlabeled,
}

/// A label name of at most [`MAX_LABEL_BYTES`] bytes, not empty, with no control character.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LabelName(String);

/// Why a label name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LabelNameError {
    /// The name is empty.
    #[error("a label name is empty")]
    Empty,
    /// The name holds a control character.
    #[error("a label name holds a control character")]
    Control,
    /// The name is over [`MAX_LABEL_BYTES`].
    #[error("a label name of {bytes} bytes is over the {MAX_LABEL_BYTES}-byte cap")]
    OverCap {
        /// The name's length in bytes.
        bytes: usize,
    },
}

impl TryFrom<String> for LabelName {
    type Error = LabelNameError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        if name.is_empty() {
            Err(LabelNameError::Empty)
        } else if name.len() > MAX_LABEL_BYTES {
            Err(LabelNameError::OverCap { bytes: name.len() })
        } else if name.chars().any(char::is_control) {
            Err(LabelNameError::Control)
        } else {
            Ok(Self(name))
        }
    }
}

impl LabelName {
    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The label a label event names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Label {
    /// A name within the cap.
    Name(LabelName),
    /// A name over [`MAX_LABEL_BYTES`]; its text is not kept, so it can be no label of assay's.
    OverCap {
        /// The name's length in bytes.
        bytes: usize,
    },
}

/// A `pull_request_target` `labeled` or `unlabeled` event's PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelPayload {
    /// The action.
    pub action: LabelAction,
    /// The label added or removed (`label.name`).
    pub label: Label,
    /// The PR number.
    pub number: NonZeroU64,
    /// The head commit (`pull_request.head.sha`).
    pub head_sha: CommitId,
    /// Where the head lives; reported, never dropped (0006).
    pub head_repository: HeadRepository,
    /// The base branch (`pull_request.base.ref`).
    pub base_ref: String,
    /// The event's sender: who added or removed the label.
    pub sender: Sender,
    /// The repository the event is for (`repository.id`, which is the base's).
    pub repository_id: NonZeroU64,
}

/// The event behind a decide-only run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideOnlyEvent {
    /// An `issue_comment` event.
    Comment(CommentPayload),
    /// A `pull_request_target` `labeled` or `unlabeled` event.
    Label(LabelPayload),
    /// A `schedule` event.
    Schedule {
        /// The repository the event is for (`repository.id`).
        repository_id: NonZeroU64,
    },
}

/// Why a decide-only event payload was refused. No comment body, and no label name or other
/// payload text past its first 64 bytes, is kept.
#[derive(Debug, thiserror::Error)]
pub enum DecideOnlyEventError {
    /// Reading the payload failed.
    #[error("reading the event payload failed")]
    Read(#[source] std::io::Error),
    /// The payload is over [`MAX_EVENT_BYTES`]; it was not parsed.
    #[error("the event payload is over the {cap}-byte cap")]
    TooLarge {
        /// The cap.
        cap: usize,
    },
    /// The payload is not JSON of the documented shape, or repeats a key.
    #[error("the event payload is not the documented shape")]
    Shape(#[source] serde_json::Error),
    /// An action decide-only does not read for its event (its first 64 bytes).
    #[error("decide-only does not run for action {0:?}")]
    Action(String),
    /// A field that does not hold what it must, by its path in the payload.
    #[error("event payload field `{0}` is missing or invalid")]
    Field(&'static str),
    /// A `sender.type` other than `User`, `Bot` and `Organization` (its first 64 bytes).
    #[error("sender type {0:?} is not one assay reads")]
    Sender(String),
}

impl From<Unread> for DecideOnlyEventError {
    fn from(unread: Unread) -> Self {
        match unread {
            Unread::Read(error) => Self::Read(error),
            Unread::TooLarge => Self::TooLarge {
                cap: MAX_EVENT_BYTES,
            },
            Unread::Shape(error) => Self::Shape(error),
        }
    }
}

impl From<InvalidField> for DecideOnlyEventError {
    fn from(InvalidField(field): InvalidField) -> Self {
        Self::Field(field)
    }
}

impl DecideOnlyEvent {
    /// Reads the payload of an `event` from `reader` (the file `GITHUB_EVENT_PATH` names), at
    /// most [`MAX_EVENT_BYTES`]. Pass `&mut reader` to keep the reader.
    ///
    /// # Errors
    ///
    /// [`DecideOnlyEventError::Read`] when reading fails; [`DecideOnlyEventError::TooLarge`]
    /// past the cap; [`DecideOnlyEventError::Shape`] for a payload that is not the documented
    /// shape or repeats a key; [`DecideOnlyEventError::Action`] for an `issue_comment` action
    /// other than `created`, `edited` and `deleted`, or a `pull_request_target` action other
    /// than `labeled` and `unlabeled`; [`DecideOnlyEventError::Sender`] for a sender type
    /// outside [`SenderKind`]; and [`DecideOnlyEventError::Field`] for a field that does not
    /// parse or contradicts another, among them a `performed_via_github_app` that is absent
    /// (it says whether an app posted the comment, so it is never assumed).
    pub fn read<R: Read>(
        event: DecideOnlyEventName,
        reader: R,
    ) -> Result<Self, DecideOnlyEventError> {
        let value = read_json(reader)?;
        match event {
            DecideOnlyEventName::IssueComment => serde_json::from_value::<CommentEventDto>(value)
                .map_err(DecideOnlyEventError::Shape)?
                .into_payload()
                .map(Self::Comment),
            DecideOnlyEventName::PullRequestTarget => {
                serde_json::from_value::<LabelEventDto>(value)
                    .map_err(DecideOnlyEventError::Shape)?
                    .into_payload()
                    .map(Self::Label)
            }
            DecideOnlyEventName::Schedule => serde_json::from_value::<ScheduleDto>(value)
                .map_err(DecideOnlyEventError::Shape)?
                .into_repository_id()
                .map(|repository_id| Self::Schedule { repository_id }),
        }
    }
}
