//! Base `.assay/owners` and `.assay/agents` (`docs/architecture/03-contracts.md` §1,
//! `docs/decisions/0005-agent-identity-and-approver.md`): the eligibility lists of 04 §5.
//!
//! `owners` and `approvers` list privileged identities only; `maintainers` lists the owner's daily
//! account, which may author and push but is never eligible; `agents` lists agent identities.
//! Numeric ids are authoritative; logins are for display. A file whose lists contradict each
//! other is refused rather than read one way or the other (wi-b5affa). This is the one parser of
//! both files (#305): `status`'s `base_identities` reads them through it too, and `doctor` through
//! `parse_unjudged`, which reports a privileged maintainer or agent as a failed prerequisite.

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use serde::Deserialize;
use thiserror::Error;

use super::error::TomlError;
use crate::status::UserId;

/// The most identities one list may hold (H-13).
pub const MAX_LIST_ENTRIES: usize = 64;

/// The largest `.assay/owners` or `.assay/agents` read, in bytes (H-13).
pub const MAX_LISTS_BYTES: usize = 64 << 10;

/// The longest GitHub login, in characters.
const MAX_LOGIN_CHARS: usize = 39;

/// A GitHub account in an eligibility list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identity {
    /// Its numeric user id, which every rule compares.
    pub id: UserId,
    /// Its login, for display only.
    pub login: String,
}

/// The four eligibility lists, each sorted by id with no id repeated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EligibilityLists {
    owners: Vec<Identity>,
    approvers: Vec<Identity>,
    maintainers: Vec<Identity>,
    agents: Vec<Identity>,
}

/// Which file a [`ListsError`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListFile {
    /// `.assay/owners`.
    Owners,
    /// `.assay/agents`.
    Agents,
}

/// One of the four lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListName {
    /// `owners`: privileged identities.
    Owners,
    /// `approvers`: privileged identities.
    Approvers,
    /// `maintainers`: never eligible.
    Maintainers,
    /// `agents`: agent identities.
    Agents,
}

impl ListName {
    /// The list's key in its file.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Owners => "owners",
            Self::Approvers => "approvers",
            Self::Maintainers => "maintainers",
            Self::Agents => "agents",
        }
    }
}

/// Why the eligibility lists were refused. The files come from the protected base.
#[derive(Debug, Error)]
pub enum ListsError {
    /// The file is larger than [`MAX_LISTS_BYTES`]; it was not parsed.
    #[error("{} is larger than {MAX_LISTS_BYTES} bytes", file.path())]
    TooLarge {
        /// The file.
        file: ListFile,
    },
    /// Not TOML, a missing or unknown key, or an entry that is not `{ id, login }` with a
    /// positive integer id.
    #[error("{} is not a valid eligibility list", file.path())]
    Toml {
        /// The file.
        file: ListFile,
        /// The parser's error.
        #[source]
        source: TomlError,
    },
    /// A list holds more than [`MAX_LIST_ENTRIES`] identities.
    #[error("`{}` lists more than {MAX_LIST_ENTRIES} identities", list.key())]
    TooMany {
        /// The list.
        list: ListName,
    },
    /// A list names one id twice.
    #[error("`{}` lists user {} twice", list.key(), id.0)]
    Repeated {
        /// The list.
        list: ListName,
        /// The id.
        id: UserId,
    },
    /// A login is not one GitHub would accept.
    #[error("`{}` gives user {} a login GitHub does not accept", list.key(), id.0)]
    Login {
        /// The list.
        list: ListName,
        /// The id the login belongs to.
        id: UserId,
    },
    /// An id is in both `owners` and `approvers`: a file that lists one account twice is
    /// refused rather than read one way (wi-b5affa).
    #[error("user {} is in both `owners` and `approvers`", id.0)]
    OwnerApprover {
        /// The id.
        id: UserId,
    },
    /// An id is a maintainer and also in `owners` or `approvers` (05 §7).
    #[error("user {} is a maintainer and also privileged", id.0)]
    MaintainerPrivileged {
        /// The id.
        id: UserId,
    },
    /// An id is an agent and also in `owners` or `approvers`: agents are never owners.
    #[error("user {} is an agent and also privileged", id.0)]
    AgentPrivileged {
        /// The id.
        id: UserId,
    },
}

impl ListFile {
    const fn path(self) -> &'static str {
        match self {
            Self::Owners => ".assay/owners",
            Self::Agents => ".assay/agents",
        }
    }
}

/// `.assay/owners` on the wire.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnersDto {
    owners: Vec<IdentityDto>,
    approvers: Vec<IdentityDto>,
    maintainers: Vec<IdentityDto>,
}

/// `.assay/agents` on the wire.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsDto {
    agents: Vec<IdentityDto>,
}

/// `github.lists` of a bundle's GitHub snapshot: the four lists the snapshot judged by.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordedLists {
    owners: Vec<IdentityDto>,
    approvers: Vec<IdentityDto>,
    maintainers: Vec<IdentityDto>,
    agents: Vec<IdentityDto>,
}

/// One `{ id, login }`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityDto {
    id: NonZeroU64,
    login: String,
}

impl EligibilityLists {
    /// Parses base `.assay/owners` and `.assay/agents`; an absent file is read as empty lists, so
    /// a repository without them has no eligible identity at all.
    ///
    /// # Errors
    ///
    /// [`ListsError`] for a file over [`MAX_LISTS_BYTES`], one that does not parse (an empty
    /// file included: every list is required), a list over [`MAX_LIST_ENTRIES`], a repeated id,
    /// a login GitHub would not accept, an owner who is also an approver, or a maintainer or
    /// agent that is also in `owners` or `approvers`.
    ///
    /// # Examples
    ///
    /// ```
    /// let owners = "owners = []\napprovers = []\nmaintainers = [{ id = 7, login = \"daily\" }]";
    /// let lists = assay_app::EligibilityLists::parse(Some(owners), None)?;
    /// assert_eq!(lists.maintainers().len(), 1);
    /// # Ok::<(), assay_app::ListsError>(())
    /// ```
    pub fn parse(owners: Option<&str>, agents: Option<&str>) -> Result<Self, ListsError> {
        Self::parse_unjudged(owners, agents)?.judged()
    }

    /// The lists a bundle's GitHub snapshot records (`github.lists`), held to every rule of
    /// [`Self::parse`]: the snapshot's lists were read by it, so a record it would refuse is
    /// not one assay wrote (wi-b5affa).
    pub(crate) fn recorded(lists: RecordedLists) -> Result<Self, ListsError> {
        Self {
            owners: identities(ListName::Owners, lists.owners)?,
            approvers: identities(ListName::Approvers, lists.approvers)?,
            maintainers: identities(ListName::Maintainers, lists.maintainers)?,
            agents: identities(ListName::Agents, lists.agents)?,
        }
        .disjoint()?
        .judged()
    }

    /// The lists, refused when a maintainer or an agent is also privileged.
    fn judged(self) -> Result<Self, ListsError> {
        if let Some(id) = self.first_privileged_in(&self.maintainers) {
            return Err(ListsError::MaintainerPrivileged { id });
        }
        if let Some(id) = self.first_privileged_in(&self.agents) {
            return Err(ListsError::AgentPrivileged { id });
        }
        Ok(self)
    }

    /// The lists, refused when an owner is also an approver.
    fn disjoint(self) -> Result<Self, ListsError> {
        match self
            .owners
            .iter()
            .map(|identity| identity.id)
            .find(|&id| find(&self.approvers, id).is_some())
        {
            Some(id) => Err(ListsError::OwnerApprover { id }),
            None => Ok(self),
        }
    }

    /// [`Self::parse`] without its two contradiction checks, a maintainer or an agent who is
    /// privileged: every other rule applies. Only `doctor` reads the lists this way, so it can
    /// report those contradictions as failed prerequisites (05 §7) rather than refuse the base;
    /// no eligibility decision may use what this returns.
    pub(crate) fn parse_unjudged(
        owners: Option<&str>,
        agents: Option<&str>,
    ) -> Result<Self, ListsError> {
        let mut lists = Self::default();
        if let Some(text) = owners {
            let dto: OwnersDto = read(ListFile::Owners, text)?;
            lists.owners = identities(ListName::Owners, dto.owners)?;
            lists.approvers = identities(ListName::Approvers, dto.approvers)?;
            lists.maintainers = identities(ListName::Maintainers, dto.maintainers)?;
        }
        if let Some(text) = agents {
            let dto: AgentsDto = read(ListFile::Agents, text)?;
            lists.agents = identities(ListName::Agents, dto.agents)?;
        }
        lists.disjoint()
    }

    /// The lists decide-only uses (R3-26): an id stays privileged only if both these lists (at
    /// `base_sha`) and `tip`'s (at the protected base's tip) hold it, so a removal takes effect
    /// at once and an addition waits for a new run; an id is a maintainer or agent if either
    /// holds it, so a restriction also takes effect at once.
    #[must_use]
    pub fn narrowed_to(&self, tip: &Self) -> Self {
        Self {
            owners: both(&self.owners, &tip.owners),
            approvers: both(&self.approvers, &tip.approvers),
            maintainers: either(&self.maintainers, &tip.maintainers),
            agents: either(&self.agents, &tip.agents),
        }
    }

    /// `owners`, sorted by id.
    #[must_use]
    pub fn owners(&self) -> &[Identity] {
        &self.owners
    }

    /// `approvers`, sorted by id.
    #[must_use]
    pub fn approvers(&self) -> &[Identity] {
        &self.approvers
    }

    /// `maintainers`, sorted by id.
    #[must_use]
    pub fn maintainers(&self) -> &[Identity] {
        &self.maintainers
    }

    /// `agents`, sorted by id.
    #[must_use]
    pub fn agents(&self) -> &[Identity] {
        &self.agents
    }

    /// The privileged identity `id` names, from `owners` first, then `approvers`.
    #[must_use]
    pub fn privileged(&self, id: UserId) -> Option<&Identity> {
        find(&self.owners, id).or_else(|| find(&self.approvers, id))
    }

    /// Whether `id` is in `owners`.
    #[must_use]
    pub fn is_owner(&self, id: UserId) -> bool {
        find(&self.owners, id).is_some()
    }

    /// Whether `id` is in `maintainers`.
    #[must_use]
    pub fn is_maintainer(&self, id: UserId) -> bool {
        find(&self.maintainers, id).is_some()
    }

    /// Whether `id` is in `agents`.
    #[must_use]
    pub fn is_agent(&self, id: UserId) -> bool {
        find(&self.agents, id).is_some()
    }

    /// The first id of `list` that is also in `owners` or `approvers`.
    fn first_privileged_in(&self, list: &[Identity]) -> Option<UserId> {
        list.iter()
            .map(|identity| identity.id)
            .find(|&id| self.privileged(id).is_some())
    }
}

/// `text` as `T`, refused unparsed past [`MAX_LISTS_BYTES`].
fn read<T: serde::de::DeserializeOwned>(file: ListFile, text: &str) -> Result<T, ListsError> {
    if text.len() > MAX_LISTS_BYTES {
        return Err(ListsError::TooLarge { file });
    }
    toml::from_str(text).map_err(|source| ListsError::Toml {
        file,
        source: TomlError(source),
    })
}

/// One list's entries, validated and sorted by id.
fn identities(list: ListName, entries: Vec<IdentityDto>) -> Result<Vec<Identity>, ListsError> {
    if entries.len() > MAX_LIST_ENTRIES {
        return Err(ListsError::TooMany { list });
    }
    let mut seen = BTreeSet::new();
    let mut identities = Vec::with_capacity(entries.len());
    for IdentityDto { id, login } in entries {
        let id = UserId(id);
        if !seen.insert(id) {
            return Err(ListsError::Repeated { list, id });
        }
        if !is_login(&login) {
            return Err(ListsError::Login { list, id });
        }
        identities.push(Identity { id, login });
    }
    identities.sort_unstable_by_key(|identity| identity.id);
    Ok(identities)
}

/// Whether `login` is 1 to 39 ASCII letters, digits and hyphens, not starting with a hyphen, as
/// GitHub allows a user's login (an App's `[bot]` login is refused: agents are users, 0005).
fn is_login(login: &str) -> bool {
    (1..=MAX_LOGIN_CHARS).contains(&login.len())
        && !login.starts_with('-')
        && login
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The identity with `id` in a list sorted by id.
fn find(list: &[Identity], id: UserId) -> Option<&Identity> {
    list.binary_search_by_key(&id, |identity| identity.id)
        .ok()
        .and_then(|index| list.get(index))
}

/// The identities of `base` whose id `tip` also holds.
fn both(base: &[Identity], tip: &[Identity]) -> Vec<Identity> {
    base.iter()
        .filter(|identity| find(tip, identity.id).is_some())
        .cloned()
        .collect()
}

/// Every identity of `base`, then those of `tip` whose id `base` lacks, sorted by id.
fn either(base: &[Identity], tip: &[Identity]) -> Vec<Identity> {
    let mut united: Vec<Identity> = base
        .iter()
        .chain(
            tip.iter()
                .filter(|identity| find(base, identity.id).is_none()),
        )
        .cloned()
        .collect();
    united.sort_unstable_by_key(|identity| identity.id);
    united
}
