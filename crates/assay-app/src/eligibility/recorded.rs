//! The GitHub state snapshot read back from a bundle (`github`, 03 §6a): strictly, as
//! [`StateRecord`](super::record::StateRecord) writes it (D3 A).
//!
//! A re-decision judges by what the evidence run recorded of the range and the pushes (no git
//! fetch), so the reader accepts exactly what the writer produces: every field, status and code
//! of its closed sets, each command line re-checked by the ledger's own rule, and nothing else
//! (wi-b5affa). The approvals and commands are checked but not kept: a re-decision reads them
//! fresh.

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use serde::Deserialize;
use serde_json::Value;

use super::approvals::IgnoredApproval;
use super::commands::{VoidCause, recorded_command};
use super::rule::{Ineligible, RangeFacts};
use super::state::CommitRange;
use super::timestamp::Timestamp;
use crate::config::{CommitId, EligibilityLists, RecordedLists, Sha256Digest};
use crate::status::UserId;

#[cfg(test)]
mod tests;

/// `github` as a bundle records it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StateRecordIn {
    lists: RecordedLists,
    author: NonZeroU64,
    head_sha: String,
    range: RangeIn,
    pushers: Vec<NonZeroU64>,
    push_clock: Option<String>,
    approvals: Vec<ApprovalIn>,
    commands: Vec<CommandIn>,
}

/// `github.range`.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum RangeIn {
    Within {
        users: Vec<NonZeroU64>,
        unattributed: bool,
    },
    NeedsSplit,
}

/// One standing approval of the head.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalIn {
    review: NonZeroU64,
    reviewer: NonZeroU64,
    submitted_at: Option<String>,
    status: ApprovalStatusIn,
    reason: Option<String>,
}

/// The statuses the writer gives an approval.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum ApprovalStatusIn {
    Counted,
    BeforePush,
    NoPushClock,
    Unsubmitted,
    Ineligible,
}

/// One command record and its status.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandIn {
    #[serde(rename = "comment_id")]
    _comment_id: NonZeroU64,
    #[serde(rename = "actor_id")]
    _actor_id: NonZeroU64,
    created_at: String,
    head_sha: String,
    body_sha256: String,
    command: String,
    args: Vec<String>,
    status: CommandStatusIn,
    reason: Option<String>,
}

/// The statuses the writer gives a command record.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum CommandStatusIn {
    Valid,
    Void,
    OtherHead,
    Ineligible,
    BeforePushClock,
}

/// What the evidence run recorded of who may act on its head: the lists it judged by, the
/// author, the range and the push records, read strictly from its bundle (D3 A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedState {
    lists: EligibilityLists,
    author: UserId,
    head: CommitId,
    pub(super) range: RangeFacts,
    pushers: BTreeSet<UserId>,
    clock: Option<Timestamp>,
}

impl RecordedState {
    /// `github` read strictly; `None` when it is not a record assay writes.
    pub(crate) fn read(github: &Value) -> Option<Self> {
        StateRecordIn::deserialize(github).ok()?.recorded()
    }

    /// The lists the evidence run judged by: base `.assay/owners` and `.assay/agents` at its
    /// `base_sha`.
    #[must_use]
    pub const fn lists(&self) -> &EligibilityLists {
        &self.lists
    }

    /// The PR's author.
    #[must_use]
    pub const fn author(&self) -> UserId {
        self.author
    }

    /// The head the snapshot was taken of.
    #[must_use]
    pub const fn head(&self) -> &CommitId {
        &self.head
    }

    /// Whether the range fit the cap.
    #[must_use]
    pub const fn commits(&self) -> CommitRange {
        match self.range {
            RangeFacts::Attributed { .. } => CommitRange::Within,
            RangeFacts::TooLarge => CommitRange::NeedsSplit,
        }
    }

    /// The senders of the PR's push records, clean merges from base apart (rule (c)).
    #[must_use]
    pub const fn pushers(&self) -> &BTreeSet<UserId> {
        &self.pushers
    }

    /// The head's push clock, `None` when no push record named it.
    #[must_use]
    pub const fn push_clock(&self) -> Option<Timestamp> {
        self.clock
    }
}

impl StateRecordIn {
    /// The head the snapshot was taken of.
    pub(crate) fn head_sha(&self) -> &str {
        &self.head_sha
    }

    /// The facts of the record, when every part of it is one the writer writes.
    fn recorded(self) -> Option<RecordedState> {
        let clock = match self.push_clock.as_deref() {
            None => None,
            Some(text) => Some(text.parse().ok()?),
        };
        if !self.approvals.iter().all(ApprovalIn::is_written) {
            return None;
        }
        if !self.commands.iter().all(CommandIn::is_written) {
            return None;
        }
        let range = match self.range {
            RangeIn::Within {
                users,
                unattributed,
            } => RangeFacts::Attributed {
                users: ids(users)?,
                unattributed,
            },
            RangeIn::NeedsSplit => RangeFacts::TooLarge,
        };
        Some(RecordedState {
            lists: EligibilityLists::recorded(self.lists).ok()?,
            author: UserId(self.author),
            head: self.head_sha.parse().ok()?,
            range,
            pushers: ids(self.pushers)?,
            clock,
        })
    }

    /// The approvals ignored because they predate the push clock, in record order, as
    /// [`GitHubState::ignored_approvals`](super::GitHubState::ignored_approvals) gave them;
    /// `None` when an ignored one's times do not read (wi-b5affa).
    pub(crate) fn ignored_approvals(&self) -> Option<Vec<IgnoredApproval>> {
        self.approvals
            .iter()
            .filter(|approval| approval.status == ApprovalStatusIn::BeforePush)
            .map(|approval| {
                Some(IgnoredApproval {
                    reviewer: UserId(approval.reviewer),
                    review: approval.review,
                    at: approval.submitted_at.as_deref()?.parse().ok()?,
                    push_at: self.push_clock.as_deref()?.parse().ok()?,
                })
            })
            .collect()
    }
}

impl ApprovalIn {
    /// Whether the writer writes this approval: its time reads, and only an ineligible one
    /// has a reason, one of [`Ineligible::code`]'s.
    fn is_written(&self) -> bool {
        absent_or_time(self.submitted_at.as_deref())
            && match (self.status, self.reason.as_deref()) {
                (ApprovalStatusIn::Ineligible, Some(code)) => ineligible(code).is_some(),
                (ApprovalStatusIn::Ineligible, None) => false,
                (_, reason) => reason.is_none(),
            }
    }
}

impl CommandIn {
    /// Whether the writer writes this record: its fields read, its command and arguments make
    /// a line the ledger's strict reader accepts, and only a void or ineligible record has a
    /// reason, one of its kind's codes.
    fn is_written(&self) -> bool {
        let mut line = format!("/assay {}", self.command);
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        let reason = match (self.status, self.reason.as_deref()) {
            (CommandStatusIn::Void, Some(code)) => void_cause(code).is_some(),
            (CommandStatusIn::Ineligible, Some(code)) => ineligible(code).is_some(),
            (CommandStatusIn::Void | CommandStatusIn::Ineligible, None) => false,
            (_, reason) => reason.is_none(),
        };
        reason
            && recorded_command(&line).is_ok()
            && self.created_at.parse::<Timestamp>().is_ok()
            && self.head_sha.parse::<CommitId>().is_ok()
            && self.body_sha256.parse::<Sha256Digest>().is_ok()
    }
}

/// Whether `text`, when present, reads as a time.
fn absent_or_time(text: Option<&str>) -> bool {
    text.is_none_or(|text| text.parse::<Timestamp>().is_ok())
}

/// The ids of a set the writer writes in order, `None` when one repeats.
fn ids(raw: Vec<NonZeroU64>) -> Option<BTreeSet<UserId>> {
    let count = raw.len();
    let set: BTreeSet<UserId> = raw.into_iter().map(UserId).collect();
    (set.len() == count).then_some(set)
}

/// The reason `code` names, the inverse of [`Ineligible::code`].
fn ineligible(code: &str) -> Option<Ineligible> {
    Some(match code {
        "not-privileged" => Ineligible::NotPrivileged,
        "maintainer" => Ineligible::Maintainer,
        "agent" => Ineligible::Agent,
        "author" => Ineligible::Author,
        "in-range" => Ineligible::InRange,
        "pusher" => Ineligible::Pusher,
        "unattributed-range" => Ineligible::UnattributedRange,
        "range-too-large" => Ineligible::RangeTooLarge,
        _ => return None,
    })
}

/// The cause `code` names, the inverse of [`VoidCause::code`].
fn void_cause(code: &str) -> Option<VoidCause> {
    Some(match code {
        "deleted" => VoidCause::Deleted,
        "edited" => VoidCause::Edited,
        "hash" => VoidCause::Hash,
        "author" => VoidCause::Author,
        "app" => VoidCause::App,
        _ => return None,
    })
}
