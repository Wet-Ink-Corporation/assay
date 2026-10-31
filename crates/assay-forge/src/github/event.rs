//! The payload of the event behind a pipeline run (#60): a `pull_request_target` event of the
//! actions assay runs for, or a `workflow_dispatch` naming only a PR (R3-14). It is GitHub's
//! wire format, so it is read here like the REST responses: a DTO, then a checked conversion
//! (OUT-01). Anything outside that shape is refused, never defaulted (wi-b5affa).

mod dto;

use std::io::Read;
use std::num::NonZeroU64;
use std::str::FromStr;

use assay_app::{CommitId, UserId};

use self::dto::{DispatchDto, PullRequestEventDto};

/// The longest payload read, in bytes: the contract cap (8 MiB, `MAX_DOCUMENT_BYTES`).
pub const MAX_EVENT_BYTES: usize = assay_app::MAX_DOCUMENT_BYTES;

/// At most this many bytes of a name read from the payload are kept in an error.
const NAME_SHOWN: usize = 64;

/// The events a pipeline run is triggered by, as `GITHUB_EVENT_NAME` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventName {
    /// `pull_request_target`.
    PullRequestTarget,
    /// `workflow_dispatch`: a requeue or a retry (06 §1).
    WorkflowDispatch,
}

/// `GITHUB_EVENT_NAME` names an event the pipeline does not run for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the pipeline runs only for `pull_request_target` and `workflow_dispatch`")]
pub struct ParseEventNameError;

impl FromStr for EventName {
    type Err = ParseEventNameError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "pull_request_target" => Ok(Self::PullRequestTarget),
            "workflow_dispatch" => Ok(Self::WorkflowDispatch),
            _ => Err(ParseEventNameError),
        }
    }
}

/// A `pull_request_target` action assay runs for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestAction {
    /// `opened`.
    Opened,
    /// `synchronize`: a push to the head branch.
    Synchronize {
        /// The head before the push (`before`).
        before: CommitId,
    },
    /// `reopened`.
    Reopened,
    /// `ready_for_review`.
    ReadyForReview,
    /// `edited`; only a base change (`changes.base`) is a reason to run.
    Edited {
        /// Whether `changes.base` is present.
        base_changed: bool,
    },
}

/// Where the PR's head branch lives, against the repository the event is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeadRepository {
    /// The base repository itself.
    Base,
    /// Another repository (a fork), by id.
    Other(NonZeroU64),
    /// GitHub reports no head repository (`head.repo: null`): it was deleted.
    Deleted,
}

/// A `pull_request_target` event's PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestPayload {
    /// The action.
    pub action: PullRequestAction,
    /// The PR number.
    pub number: NonZeroU64,
    /// The head commit (`pull_request.head.sha`).
    pub head_sha: CommitId,
    /// The base commit (`pull_request.base.sha`).
    pub base_sha: CommitId,
    /// The base branch (`pull_request.base.ref`).
    pub base_ref: String,
    /// Whether the PR is a draft.
    pub draft: bool,
    /// The PR's author (`pull_request.user.id`).
    pub author: UserId,
    /// The event's sender (`sender.id`).
    pub sender: UserId,
    /// The repository the event is for (`repository.id`, which is the base's).
    pub repository_id: NonZeroU64,
    /// Where the head lives; reported, never dropped (0006).
    pub head_repository: HeadRepository,
    /// The repository's default branch.
    pub default_branch: String,
}

/// A `workflow_dispatch` event whose only input is `pr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchPayload {
    /// The PR number (`inputs.pr`).
    pub pr: NonZeroU64,
    /// The event's sender.
    pub sender: UserId,
    /// The repository the event is for.
    pub repository_id: NonZeroU64,
    /// The repository's default branch.
    pub default_branch: String,
}

/// The event behind a pipeline run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPayload {
    /// A `pull_request_target` event.
    PullRequest(PullRequestPayload),
    /// A `workflow_dispatch` event.
    Dispatch(DispatchPayload),
}

/// Why the event payload was refused.
#[derive(Debug, thiserror::Error)]
pub enum EventPayloadError {
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
    /// A `pull_request_target` action assay does not run for (its first 64 bytes).
    #[error("assay does not run for pull_request_target action {0:?}")]
    Action(String),
    /// A field that does not hold what it must, by its path in the payload.
    #[error("event payload field `{0}` is missing or invalid")]
    Field(&'static str),
    /// A `workflow_dispatch` input other than `pr` (its name's first 64 bytes): no role, cap
    /// or factor is accepted from a dispatch (R3-14).
    #[error("workflow_dispatch input {0:?} is not accepted; only `pr` is (R3-14)")]
    DispatchInput(String),
}

impl EventPayload {
    /// Reads the payload of an `event` from `reader` (the file `GITHUB_EVENT_PATH` names),
    /// at most [`MAX_EVENT_BYTES`]. Pass `&mut reader` to keep the reader.
    ///
    /// # Errors
    ///
    /// [`EventPayloadError::Read`] when reading fails; [`EventPayloadError::TooLarge`] past
    /// the cap; [`EventPayloadError::Shape`] for a payload that is not the documented shape
    /// or repeats a key; [`EventPayloadError::Action`] for an action other than `opened`,
    /// `synchronize`, `reopened`, `ready_for_review` and `edited`;
    /// [`EventPayloadError::DispatchInput`] for a dispatch input other than `pr`; and
    /// [`EventPayloadError::Field`] for a field that does not parse or contradicts another.
    pub fn read<R: Read>(event: EventName, reader: R) -> Result<Self, EventPayloadError> {
        let limit = u64::try_from(MAX_EVENT_BYTES).map_or(u64::MAX, |cap| cap.saturating_add(1));
        let mut body = Vec::new();
        reader
            .take(limit)
            .read_to_end(&mut body)
            .map_err(EventPayloadError::Read)?;
        if body.len() > MAX_EVENT_BYTES {
            return Err(EventPayloadError::TooLarge {
                cap: MAX_EVENT_BYTES,
            });
        }
        let value = assay_app::parse_json_strict(&body).map_err(EventPayloadError::Shape)?;
        match event {
            EventName::PullRequestTarget => serde_json::from_value::<PullRequestEventDto>(value)
                .map_err(EventPayloadError::Shape)?
                .into_payload()
                .map(Self::PullRequest),
            EventName::WorkflowDispatch => serde_json::from_value::<DispatchDto>(value)
                .map_err(EventPayloadError::Shape)?
                .into_payload()
                .map(Self::Dispatch),
        }
    }
}

/// `id` as a positive id, or the refusal of `field`.
fn positive(id: u64, field: &'static str) -> Result<NonZeroU64, EventPayloadError> {
    NonZeroU64::new(id).ok_or(EventPayloadError::Field(field))
}

/// `sha` as a full commit id, or the refusal of `field`.
fn commit(sha: &str, field: &'static str) -> Result<CommitId, EventPayloadError> {
    sha.parse().ok().ok_or(EventPayloadError::Field(field))
}

/// `name` as a branch name (not empty, no control character), or the refusal of `field`.
fn branch(name: String, field: &'static str) -> Result<String, EventPayloadError> {
    if name.is_empty() || name.chars().any(char::is_control) {
        Err(EventPayloadError::Field(field))
    } else {
        Ok(name)
    }
}

/// `text` as a positive number in canonical decimal: ASCII digits, the first not `0`.
fn canonical_positive(text: &str) -> Option<NonZeroU64> {
    let canonical = text
        .bytes()
        .next()
        .is_some_and(|first| matches!(first, b'1'..=b'9'))
        && text.bytes().all(|byte| byte.is_ascii_digit());
    if canonical { text.parse().ok() } else { None }
}

/// `name`, cut at a character boundary to [`NAME_SHOWN`] bytes, for an error.
fn shown(name: &str) -> String {
    name.char_indices()
        .take_while(|&(start, char)| start.saturating_add(char.len_utf8()) <= NAME_SHOWN)
        .map(|(_, char)| char)
        .collect()
}
