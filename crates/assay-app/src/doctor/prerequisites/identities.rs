//! The P10 identity lists of base config (03 §1): `.assay/owners` (`owners`, `approvers`,
//! `maintainers`, each `[{id, login}]`) and `.assay/agents` (`agents = [{id, login}]`). Numeric
//! ids are authoritative; logins are for display and not kept.
//!
//! #58 fixes the format of `.assay/agents`; until then it mirrors `.assay/owners` (a two-way
//! default of #75).

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use serde::Deserialize;
use thiserror::Error;

use crate::config::TomlError;
use crate::status::UserId;

/// The most ids one list holds (H-13).
pub const MAX_LISTED: usize = 256;

/// The identity lists, by numeric GitHub user id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityLists {
    pub(super) owners: BTreeSet<UserId>,
    pub(super) approvers: BTreeSet<UserId>,
    pub(super) maintainers: BTreeSet<UserId>,
    pub(super) agents: BTreeSet<UserId>,
}

impl IdentityLists {
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

/// Why an identity list was refused.
#[derive(Debug, Error)]
pub enum IdentityListError {
    /// Not TOML of the documented shape: an unknown key, a missing `id`, a wrong type.
    #[error("not a valid identity list")]
    Syntax(#[source] TomlError),
    /// An id is zero.
    #[error("`{list}` lists the id 0")]
    ZeroId {
        /// The list.
        list: &'static str,
    },
    /// An entry's `login` is empty.
    #[error("`{list}` gives {id} an empty login")]
    NoLogin {
        /// The list.
        list: &'static str,
        /// The id.
        id: u64,
    },
    /// An id is listed twice in one list.
    #[error("`{list}` lists {id} twice")]
    Repeated {
        /// The list.
        list: &'static str,
        /// The id.
        id: u64,
    },
    /// A list holds more than [`MAX_LISTED`] ids.
    #[error("`{list}` lists more than {MAX_LISTED} ids")]
    TooMany {
        /// The list.
        list: &'static str,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnersDto {
    #[serde(default)]
    owners: Vec<EntryDto>,
    #[serde(default)]
    approvers: Vec<EntryDto>,
    #[serde(default)]
    maintainers: Vec<EntryDto>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsDto {
    agents: Vec<EntryDto>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryDto {
    id: u64,
    /// For display only: required and non-empty, but not kept.
    login: String,
}

impl IdentityLists {
    /// Reads `.assay/owners` from `owners` and `.assay/agents` from `agents`.
    ///
    /// # Errors
    ///
    /// [`IdentityListError`] when either is not the documented TOML, or a list holds a zero
    /// or repeated id or more than [`MAX_LISTED`] ids.
    pub fn parse(owners: &str, agents: &str) -> Result<Self, IdentityListError> {
        let syntax = |error| IdentityListError::Syntax(TomlError(error));
        let lists: OwnersDto = toml::from_str(owners).map_err(syntax)?;
        let listed: AgentsDto = toml::from_str(agents).map_err(syntax)?;
        Ok(Self {
            owners: ids("owners", lists.owners)?,
            approvers: ids("approvers", lists.approvers)?,
            maintainers: ids("maintainers", lists.maintainers)?,
            agents: ids("agents", listed.agents)?,
        })
    }
}

/// The ids of `entries`, refused when zero, repeated or too many.
fn ids(list: &'static str, entries: Vec<EntryDto>) -> Result<BTreeSet<UserId>, IdentityListError> {
    if entries.len() > MAX_LISTED {
        return Err(IdentityListError::TooMany { list });
    }
    let mut ids = BTreeSet::new();
    for entry in entries {
        let id = NonZeroU64::new(entry.id).ok_or(IdentityListError::ZeroId { list })?;
        if entry.login.is_empty() {
            return Err(IdentityListError::NoLogin { list, id: entry.id });
        }
        if !ids.insert(UserId(id)) {
            return Err(IdentityListError::Repeated { list, id: entry.id });
        }
    }
    Ok(ids)
}
