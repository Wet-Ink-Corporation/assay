//! Base `.assay/owners` and `.assay/agents`: the P10 identity lists, by numeric GitHub user id
//! (`docs/architecture/03-contracts.md` §1). Logins are for display; ids are authoritative.
//!
//! `.assay/owners` holds `owners`, `approvers` and `maintainers`, each a list of `{id, login}`;
//! `owners` and `approvers` are the privileged identities. `.assay/agents` holds `agents` in the
//! same shape (#58 fixes its format; until then this strict reading is the one accepted, and
//! any other is refused). Unknown keys, a zero id, a login GitHub would not give and an id
//! listed twice in one file are refused (wi-b5affa).

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use serde::Deserialize;
use thiserror::Error;

use super::error::TomlError;
use crate::status::UserId;

/// The longest GitHub login, without a `[bot]` suffix.
const MAX_LOGIN_BYTES: usize = 39;

/// Why `.assay/owners` or `.assay/agents` was refused. The file comes from the protected base,
/// so errors may quote it.
#[derive(Debug, Error)]
pub enum IdentitiesConfigError {
    /// Not TOML, an unknown key, or a value of the wrong type.
    #[error("not a valid identity list")]
    Syntax(#[source] TomlError),
    /// An entry's id is 0.
    #[error("an id is 0")]
    ZeroId,
    /// An entry's login is not one GitHub gives (1 to 39 letters, digits and `-`, not starting
    /// with `-`, optionally `[bot]`).
    #[error("`{login}` is not a GitHub login")]
    Login {
        /// The login given.
        login: String,
    },
    /// An id is listed twice in the file: in one list, or in two of `owners`, `approvers` and
    /// `maintainers` (a maintainer is never privileged, 03 §1).
    #[error("id {id} is listed twice")]
    Repeated {
        /// The id.
        id: u64,
    },
}

/// The wire shape of `.assay/owners`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnersDto {
    #[serde(default)]
    owners: Vec<IdentityDto>,
    #[serde(default)]
    approvers: Vec<IdentityDto>,
    #[serde(default)]
    maintainers: Vec<IdentityDto>,
}

/// The wire shape of `.assay/agents`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsDto {
    #[serde(default)]
    agents: Vec<IdentityDto>,
}

/// One listed identity.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityDto {
    id: u64,
    login: String,
}

/// Parses base `.assay/owners`: the privileged ids (`owners` and `approvers`); `maintainers`
/// are checked and left out.
pub(crate) fn parse_owners(text: &str) -> Result<BTreeSet<UserId>, IdentitiesConfigError> {
    let dto: OwnersDto =
        toml::from_str(text).map_err(|e| IdentitiesConfigError::Syntax(TomlError(e)))?;
    let mut seen = BTreeSet::new();
    let privileged = ids(dto.owners.into_iter().chain(dto.approvers), &mut seen)?;
    ids(dto.maintainers, &mut seen)?;
    Ok(privileged)
}

/// Parses base `.assay/agents`: the agent ids.
pub(crate) fn parse_agents(text: &str) -> Result<BTreeSet<UserId>, IdentitiesConfigError> {
    let dto: AgentsDto =
        toml::from_str(text).map_err(|e| IdentitiesConfigError::Syntax(TomlError(e)))?;
    ids(dto.agents, &mut BTreeSet::new())
}

/// The ids of `entries`, each checked and recorded in `seen`, which none may already hold.
fn ids(
    entries: impl IntoIterator<Item = IdentityDto>,
    seen: &mut BTreeSet<UserId>,
) -> Result<BTreeSet<UserId>, IdentitiesConfigError> {
    let mut listed = BTreeSet::new();
    for entry in entries {
        let id = UserId(NonZeroU64::new(entry.id).ok_or(IdentitiesConfigError::ZeroId)?);
        if !is_login(&entry.login) {
            return Err(IdentitiesConfigError::Login { login: entry.login });
        }
        if !seen.insert(id) {
            return Err(IdentitiesConfigError::Repeated { id: entry.id });
        }
        listed.insert(id);
    }
    Ok(listed)
}

/// Whether `login` is one GitHub gives: 1 to 39 ASCII letters, digits and `-`, not starting
/// with `-`, optionally followed by `[bot]`.
fn is_login(login: &str) -> bool {
    let name = login.strip_suffix("[bot]").unwrap_or(login);
    (1..=MAX_LOGIN_BYTES).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}
