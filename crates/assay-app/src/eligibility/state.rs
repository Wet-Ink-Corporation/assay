//! The GitHub state snapshot (04 §5 "Inputs"): what one decision knows about who may act on the
//! head, and the facts the rubric reads from it.

use std::collections::BTreeSet;

use assay_domain::{Authorship, Override, OwnerApproval, Readiness, Waiver};

use super::approvals::{Approval, ApprovalStatus, IgnoredApproval};
use super::commands::{
    CommandRecord, CommandRejected, CommandStatus, CommentEvent, JudgedCommand, Recorded,
    VoidCause, record,
};
use super::grammar::Command;
use super::pushes::PushRecord;
use super::rule::{Ineligible, Rule};
use super::timestamp::Timestamp;
use crate::config::{CommitId, EligibilityLists};
use crate::decide::Snapshot;
use crate::status::UserId;

/// Whether the range fits the eligibility rule's cap (R3-22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitRange {
    /// At most 250 commits: every one was attributed.
    Within,
    /// More than 250 commits: the PR is `needs-split` (rule 3).
    NeedsSplit,
}

/// The snapshot of one PR head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubState {
    pub(super) head: CommitId,
    pub(super) rule: Rule,
    pub(super) commits: CommitRange,
    pub(super) clock: Option<Timestamp>,
    pub(super) pushes: Vec<PushRecord>,
    pub(super) approvals: Vec<Approval>,
    pub(super) commands: Vec<JudgedCommand>,
}

impl GitHubState {
    /// `owner_approved(head)`: some eligible reviewer's standing approval of the head postdates
    /// the push clock.
    #[must_use]
    pub fn owner_approval(&self) -> OwnerApproval {
        if self
            .approvals
            .iter()
            .any(|approval| approval.status == ApprovalStatus::Counted)
        {
            OwnerApproval::Approved
        } else {
            OwnerApproval::NotApproved
        }
    }

    /// Whether an agent identity (`.assay/agents`) authored the PR.
    #[must_use]
    pub fn authorship(&self) -> Authorship {
        if self.rule.lists.is_agent(self.rule.author) {
            Authorship::Agent
        } else {
            Authorship::Other
        }
    }

    /// Whether `id` may take a privileged action on this head.
    ///
    /// # Errors
    ///
    /// The first [`Ineligible`] reason that applies.
    pub fn eligibility(&self, id: UserId) -> Result<(), Ineligible> {
        self.rule.judge(id)
    }

    /// The push clock of the head, `None` when no push record names it.
    #[must_use]
    pub const fn push_clock(&self) -> Option<Timestamp> {
        self.clock
    }

    /// The senders of push records for the PR, clean merges from base apart (rule (c)).
    #[must_use]
    pub const fn pushers(&self) -> &BTreeSet<UserId> {
        &self.rule.pushers
    }

    /// Whether the range fits the cap.
    #[must_use]
    pub const fn commits(&self) -> CommitRange {
        self.commits
    }

    /// The lists the rule read.
    #[must_use]
    pub const fn lists(&self) -> &EligibilityLists {
        &self.rule.lists
    }

    /// The approvals of the head ignored because they predate the push clock, for the
    /// summary.
    pub fn ignored_approvals(&self) -> impl Iterator<Item = IgnoredApproval> + '_ {
        self.approvals.iter().filter_map(Approval::ignored)
    }

    /// The snapshot facts `decide` reads, with the PR's draft flag and requeue count read
    /// elsewhere.
    #[must_use]
    pub fn snapshot(&self, readiness: Readiness, requeues: u32) -> Snapshot {
        Snapshot {
            owner_approval: self.owner_approval(),
            authorship: self.authorship(),
            readiness,
            requeues,
            commits: self.commits,
        }
    }

    /// The command decide-only records for the triggering comment `event`, judged on this
    /// head; or why it records none.
    ///
    /// # Errors
    ///
    /// [`CommandRejected`] for an event that is not `created`, a comment an app posted, text
    /// that is not a command of the grammar, an ineligible author, or a comment not later than
    /// the push clock.
    pub fn record_command(&self, event: &CommentEvent) -> Result<Recorded, CommandRejected> {
        record(event, &self.head, self.clock, &self.rule)
    }

    /// The valid waivers of this head, for [`DecisionInputs::waivers`](assay_domain::DecisionInputs).
    #[must_use]
    pub fn waivers(&self) -> Vec<Waiver> {
        self.valid()
            .filter_map(|record| match &record.command {
                Command::Waive { finding } => Some(Waiver {
                    fp: *finding,
                    by: self.login(record.actor),
                }),
                _ => None,
            })
            .collect()
    }

    /// The latest valid override of this head.
    #[must_use]
    pub fn override_record(&self) -> Option<Override> {
        self.valid()
            .filter(|record| matches!(record.command, Command::Override { .. }))
            .max_by_key(|record| (record.created_at, record.comment_id))
            .and_then(|record| match &record.command {
                Command::Override {
                    scope,
                    reasons_digest,
                    ..
                } => Some(Override {
                    scope: *scope,
                    reasons_digest: reasons_digest.as_str().to_owned(),
                    by: self.login(record.actor),
                }),
                _ => None,
            })
    }

    /// The valid dismissals of the PR, for the context pack.
    pub fn dismissals(&self) -> impl Iterator<Item = &CommandRecord> + '_ {
        self.valid()
            .filter(|record| matches!(record.command, Command::Dismiss { .. }))
    }

    /// The command records whose comments no longer stand, for `CommandVoided` events.
    pub fn voided(&self) -> impl Iterator<Item = (&CommandRecord, VoidCause)> + '_ {
        self.commands
            .iter()
            .filter_map(|judged| match judged.status {
                CommandStatus::Void(cause) => Some((&judged.record, cause)),
                _ => None,
            })
    }

    /// The records that apply to this decision.
    fn valid(&self) -> impl Iterator<Item = &CommandRecord> + '_ {
        self.commands
            .iter()
            .filter(|judged| judged.status == CommandStatus::Valid)
            .map(|judged| &judged.record)
    }

    /// The login the lists give a privileged `id`, as the decision records who acted.
    fn login(&self, id: UserId) -> String {
        self.rule
            .lists
            .privileged(id)
            .map_or_else(|| id.0.to_string(), |identity| identity.login.clone())
    }
}
