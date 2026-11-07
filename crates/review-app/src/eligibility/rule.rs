//! The one eligibility rule (04 §5), used verbatim by approval, waive, override, dismiss, retry
//! and Weigh-In ack (R-07, R-08, P10, T9, ADR-16).

use std::collections::BTreeSet;

use crate::config::EligibilityLists;
use crate::status::UserId;

/// Why an id may take no privileged action on this PR head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum Ineligible {
    /// It is in neither `owners` nor `approvers`.
    #[error("not in owners or approvers")]
    NotPrivileged,
    /// It is a maintainer, such as the owner's daily account (R3-01).
    #[error("a maintainer, never eligible")]
    Maintainer,
    /// It is an agent identity.
    #[error("an agent identity")]
    Agent,
    /// It authored the PR (a).
    #[error("the PR author")]
    Author,
    /// It authored or committed a commit of the range (b), clean merges from base apart.
    #[error("an author or committer of the range")]
    InRange,
    /// It sent a push record for the PR (c), clean merges from base apart.
    #[error("a pusher of the PR")]
    Pusher,
    /// It is an owner and a commit of the range has an author or committer that is no GitHub
    /// user, which counts as authored by every owner.
    #[error("an owner, and a commit of the range is not attributed to a GitHub user")]
    UnattributedRange,
    /// The range holds more than 250 commits, so (b) cannot be judged (`needs-split`).
    #[error("the range is too large to attribute")]
    RangeTooLarge,
}

/// What the range said about (b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RangeFacts {
    /// Every commit was attributed, clean merges from base apart.
    Attributed {
        /// GitHub users who authored or committed a counted commit.
        users: BTreeSet<UserId>,
        /// Whether a counted commit is not attributed to a GitHub user.
        unattributed: bool,
    },
    /// Past the cap: nothing is attributed and nobody is eligible.
    TooLarge,
}

/// The facts the rule judges an id against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub(crate) lists: EligibilityLists,
    pub(crate) author: UserId,
    pub(crate) range: RangeFacts,
    pub(crate) pushers: BTreeSet<UserId>,
}

impl Rule {
    /// Whether `id` is eligible, or the first reason it is not.
    pub(crate) fn judge(&self, id: UserId) -> Result<(), Ineligible> {
        self.judge_lists(id)?;
        if id == self.author {
            return Err(Ineligible::Author);
        }
        match &self.range {
            RangeFacts::TooLarge => return Err(Ineligible::RangeTooLarge),
            RangeFacts::Attributed { users, .. } if users.contains(&id) => {
                return Err(Ineligible::InRange);
            }
            RangeFacts::Attributed { unattributed, .. } => {
                if *unattributed && self.lists.is_owner(id) {
                    return Err(Ineligible::UnattributedRange);
                }
            }
        }
        if self.pushers.contains(&id) {
            return Err(Ineligible::Pusher);
        }
        Ok(())
    }

    /// The lists' part of the rule alone: privileged, and neither a maintainer nor an agent.
    pub(crate) fn judge_lists(&self, id: UserId) -> Result<(), Ineligible> {
        if self.lists.is_maintainer(id) {
            return Err(Ineligible::Maintainer);
        }
        if self.lists.is_agent(id) {
            return Err(Ineligible::Agent);
        }
        if self.lists.privileged(id).is_none() {
            return Err(Ineligible::NotPrivileged);
        }
        Ok(())
    }
}
