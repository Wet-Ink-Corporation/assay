//! The P10 identity lists of base config (03 §1): `.assay/owners` (`owners`, `approvers`,
//! `maintainers`, each `[{id, login}]`) and `.assay/agents` (`agents = [{id, login}]`). Numeric
//! ids are authoritative; logins are for display and not kept.
//!
//! The files are parsed by `EligibilityLists`' parser (#305), the one parser of both files, so
//! doctor refuses every file a run refuses. A maintainer or agent who is privileged is left to
//! the `identity-lists` prerequisite, which reports it as a failure rather than refusing the base.

use std::collections::BTreeSet;

use crate::config::{EligibilityLists, Identity, ListsError};
use crate::status::UserId;

/// The identity lists, by numeric GitHub user id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityLists {
    pub(super) owners: BTreeSet<UserId>,
    pub(super) approvers: BTreeSet<UserId>,
    pub(super) maintainers: BTreeSet<UserId>,
    pub(super) agents: BTreeSet<UserId>,
}

impl IdentityLists {
    /// Reads `.assay/owners` from `owners` and `.assay/agents` from `agents`, by every rule of
    /// [`EligibilityLists::parse`] except that a maintainer or agent who is also privileged is
    /// read, for the `identity-lists` prerequisite to fail on.
    ///
    /// # Errors
    ///
    /// [`ListsError`] when either file is refused.
    pub fn parse(owners: &str, agents: &str) -> Result<Self, ListsError> {
        let lists = EligibilityLists::parse_unjudged(Some(owners), Some(agents))?;
        Ok(Self {
            owners: ids(lists.owners()),
            approvers: ids(lists.approvers()),
            maintainers: ids(lists.maintainers()),
            agents: ids(lists.agents()),
        })
    }

    /// `owners`: privileged accounts, such as the break-glass account.
    #[must_use]
    pub const fn owners(&self) -> &BTreeSet<UserId> {
        &self.owners
    }

    /// `approvers`: the dedicated approver account (H-13).
    #[must_use]
    pub const fn approvers(&self) -> &BTreeSet<UserId> {
        &self.approvers
    }

    /// `maintainers`: the owner's daily account, which takes no privileged action.
    #[must_use]
    pub const fn maintainers(&self) -> &BTreeSet<UserId> {
        &self.maintainers
    }

    /// `.assay/agents`.
    #[must_use]
    pub const fn agents(&self) -> &BTreeSet<UserId> {
        &self.agents
    }
}

/// The ids of `list`.
fn ids(list: &[Identity]) -> BTreeSet<UserId> {
    list.iter().map(|identity| identity.id).collect()
}
