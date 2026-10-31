//! The GitHub state snapshot as the `assay.run-inputs/1` bundle records it (`github`, 03 §6a,
//! 02 §5): the eligibility lists judged by, what the range and the push records said, every
//! standing approval and every command record with what this decision made of it.

use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use super::approvals::{Approval, ApprovalStatus, IgnoredApproval};
use super::commands::{CommandStatus, JudgedCommand};
use super::rule::{Ineligible, RangeFacts};
use super::state::GitHubState;
use super::timestamp::Timestamp;
use crate::config::{EligibilityLists, Identity};
use crate::status::UserId;

/// `github`.
#[derive(Serialize)]
pub(crate) struct StateRecord<'a> {
    lists: ListsRecord<'a>,
    author: u64,
    head_sha: &'a str,
    range: RangeRecord,
    pushers: Vec<u64>,
    push_clock: Option<Timestamp>,
    approvals: Vec<ApprovalRecord>,
    commands: Vec<CommandOut<'a>>,
}

/// `github.lists`.
#[derive(Serialize)]
struct ListsRecord<'a> {
    owners: Vec<IdentityRecord<'a>>,
    approvers: Vec<IdentityRecord<'a>>,
    maintainers: Vec<IdentityRecord<'a>>,
    agents: Vec<IdentityRecord<'a>>,
}

/// One `{id, login}`.
#[derive(Serialize)]
struct IdentityRecord<'a> {
    id: u64,
    login: &'a str,
}

/// `github.range`: what rule (b) read from the range.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum RangeRecord {
    /// Every commit attributed, clean merges from base apart.
    Within { users: Vec<u64>, unattributed: bool },
    /// Past 250 commits.
    NeedsSplit,
}

/// One standing approval of the head.
#[derive(Serialize)]
struct ApprovalRecord {
    review: u64,
    reviewer: u64,
    submitted_at: Option<Timestamp>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// One command record and its status.
#[derive(Serialize)]
struct CommandOut<'a> {
    comment_id: u64,
    actor_id: u64,
    created_at: Timestamp,
    head_sha: &'a str,
    body_sha256: &'a str,
    command: &'static str,
    args: Vec<String>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// `github` as `publish_persisted` reads it back from a bundle `decide` wrote: the head it is
/// of and the approvals ignored for timing, which the summary lists. The parts it does not read
/// are left to the bundle's reader.
#[derive(Deserialize)]
pub(crate) struct StateRecordIn {
    head_sha: String,
    push_clock: Option<String>,
    approvals: Vec<ApprovalIn>,
}

/// One standing approval, as [`ApprovalRecord`] writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalIn {
    review: NonZeroU64,
    reviewer: NonZeroU64,
    submitted_at: Option<String>,
    status: String,
    /// Allowed beside an `ineligible` status; not read.
    #[serde(rename = "reason")]
    _reason: Option<String>,
}

/// The statuses [`ApprovalRecord`] writes.
const APPROVAL_STATUSES: [&str; 5] = [
    "counted",
    "before-push",
    "no-push-clock",
    "unsubmitted",
    "ineligible",
];

impl StateRecordIn {
    /// The head the snapshot was taken of.
    pub(crate) fn head_sha(&self) -> &str {
        &self.head_sha
    }

    /// The approvals ignored because they predate the push clock, in record order, as
    /// [`GitHubState::ignored_approvals`] gave them; `None` when an approval's status is not one
    /// assay writes or an ignored one's times do not read (wi-b5affa).
    pub(crate) fn ignored_approvals(&self) -> Option<Vec<IgnoredApproval>> {
        let mut ignored = Vec::new();
        for approval in &self.approvals {
            if !APPROVAL_STATUSES.contains(&approval.status.as_str()) {
                return None;
            }
            if approval.status != "before-push" {
                continue;
            }
            ignored.push(IgnoredApproval {
                reviewer: UserId(approval.reviewer),
                review: approval.review,
                at: approval.submitted_at.as_deref()?.parse().ok()?,
                push_at: self.push_clock.as_deref()?.parse().ok()?,
            });
        }
        Some(ignored)
    }
}

impl Ineligible {
    /// The reason's code in the bundle.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotPrivileged => "not-privileged",
            Self::Maintainer => "maintainer",
            Self::Agent => "agent",
            Self::Author => "author",
            Self::InRange => "in-range",
            Self::Pusher => "pusher",
            Self::UnattributedRange => "unattributed-range",
            Self::RangeTooLarge => "range-too-large",
        }
    }
}

impl<'a> StateRecord<'a> {
    /// The record of `state`.
    pub(crate) fn of(state: &'a GitHubState) -> Self {
        let rule = &state.rule;
        Self {
            lists: lists(&rule.lists),
            author: rule.author.0.get(),
            head_sha: state.head.as_str(),
            range: match &rule.range {
                RangeFacts::Attributed {
                    users,
                    unattributed,
                } => RangeRecord::Within {
                    users: users.iter().map(|id| id.0.get()).collect(),
                    unattributed: *unattributed,
                },
                RangeFacts::TooLarge => RangeRecord::NeedsSplit,
            },
            pushers: rule.pushers.iter().map(|id| id.0.get()).collect(),
            push_clock: state.clock,
            approvals: state.approvals.iter().map(approval).collect(),
            commands: state.commands.iter().map(command).collect(),
        }
    }
}

fn list(identities: &[Identity]) -> Vec<IdentityRecord<'_>> {
    identities
        .iter()
        .map(|identity| IdentityRecord {
            id: identity.id.0.get(),
            login: &identity.login,
        })
        .collect()
}

fn lists(lists: &EligibilityLists) -> ListsRecord<'_> {
    ListsRecord {
        owners: list(lists.owners()),
        approvers: list(lists.approvers()),
        maintainers: list(lists.maintainers()),
        agents: list(lists.agents()),
    }
}

fn approval(approval: &Approval) -> ApprovalRecord {
    let (status, reason) = match approval.status {
        ApprovalStatus::Counted => ("counted", None),
        ApprovalStatus::BeforePush(_) => ("before-push", None),
        ApprovalStatus::NoPushClock => ("no-push-clock", None),
        ApprovalStatus::Unsubmitted => ("unsubmitted", None),
        ApprovalStatus::Ineligible(why) => ("ineligible", Some(why.code())),
    };
    ApprovalRecord {
        review: approval.review.get(),
        reviewer: approval.reviewer.0.get(),
        submitted_at: approval.submitted_at,
        status,
        reason,
    }
}

fn command(judged: &JudgedCommand) -> CommandOut<'_> {
    let record = &judged.record;
    let (status, reason) = match judged.status {
        CommandStatus::Valid => ("valid", None),
        CommandStatus::Void(cause) => ("void", Some(cause.code())),
        CommandStatus::OtherHead => ("other-head", None),
        CommandStatus::Ineligible(why) => ("ineligible", Some(why.code())),
        CommandStatus::BeforePushClock => ("before-push-clock", None),
    };
    CommandOut {
        comment_id: record.comment_id.0.get(),
        actor_id: record.actor.0.get(),
        created_at: record.created_at,
        head_sha: record.head_sha.as_str(),
        body_sha256: record.body_sha256.as_str(),
        command: record.command.name(),
        args: record.command.args(),
        status,
        reason,
    }
}
