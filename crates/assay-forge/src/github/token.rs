//! App installation tokens minted narrowed per job: only the job's repositories and
//! permissions (05 §4), and refused when GitHub grants anything else.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::api::{Api, decode, encode};
use super::error::{ForgeError, Operation};
use super::repository::is_part;
use super::transport::{Request, Sleep, Transport};

/// The most repositories one token may cover; GitHub's own limit is 500.
const MAX_REPOSITORIES: usize = 500;

/// An App installation's numeric id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstallationId(pub NonZeroU64);

/// A repository permission a job's token can hold; only those assay's jobs use (05 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Permission {
    /// `actions`: reading runs and artifacts, dispatching a requeue.
    Actions,
    /// `checks`: the verdict and per-gate checks.
    Checks,
    /// `contents`: base objects, and the ledger repository.
    Contents,
    /// `metadata`: granted to every token anyway.
    Metadata,
    /// `pull_requests`: comments, reviews, PR state.
    PullRequests,
}

impl Permission {
    /// The permission's wire name.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Actions => "actions",
            Self::Checks => "checks",
            Self::Contents => "contents",
            Self::Metadata => "metadata",
            Self::PullRequests => "pull_requests",
        }
    }
}

/// How much of a permission a token holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Access {
    /// `read`.
    Read,
    /// `write`.
    Write,
}

impl Access {
    /// The access level's wire name.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// What a job's token may reach: named repositories of the installation and named
/// permissions. Never empty in either, since GitHub reads an empty list as "everything the
/// installation has".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenScope {
    repositories: Vec<String>,
    permissions: BTreeMap<Permission, Access>,
}

/// Why a token scope was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    /// No repository was named: the token would cover every repository of the installation.
    #[error("a token scope names no repository")]
    NoRepository,
    /// More than 500 repositories.
    #[error("a token scope names more than {MAX_REPOSITORIES} repositories")]
    TooManyRepositories,
    /// A repository name is not 1 to 100 of `A-Z a-z 0-9 . _ -`.
    #[error("a repository name in a token scope is invalid")]
    InvalidName,
    /// No permission was named: the token would hold every permission of the installation.
    #[error("a token scope names no permission")]
    NoPermission,
}

impl TokenScope {
    /// A scope over `repositories` (names, without the owner) holding `permissions`; a
    /// permission named twice keeps its last access.
    ///
    /// # Errors
    ///
    /// [`ScopeError`] when either list is empty, a name is invalid, or there are more than 500
    /// repositories.
    pub fn new<'a>(
        repositories: impl IntoIterator<Item = &'a str>,
        permissions: impl IntoIterator<Item = (Permission, Access)>,
    ) -> Result<Self, ScopeError> {
        let mut names = Vec::new();
        for name in repositories {
            if !is_part(name) {
                return Err(ScopeError::InvalidName);
            }
            if names.len() >= MAX_REPOSITORIES {
                return Err(ScopeError::TooManyRepositories);
            }
            names.push(name.to_owned());
        }
        names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
        names.dedup_by(|later, earlier| later.eq_ignore_ascii_case(earlier));
        if names.is_empty() {
            return Err(ScopeError::NoRepository);
        }
        let permissions: BTreeMap<_, _> = permissions.into_iter().collect();
        if permissions.is_empty() {
            return Err(ScopeError::NoPermission);
        }
        Ok(Self {
            repositories: names,
            permissions,
        })
    }

    /// The request body: `{"repositories": [...], "permissions": {...}}`.
    fn body(&self) -> Value {
        let permissions: Map<String, Value> = self
            .permissions
            .iter()
            .map(|(permission, access)| (permission.code().to_owned(), Value::from(access.code())))
            .collect();
        let mut body = Map::new();
        body.insert(
            "repositories".to_owned(),
            Value::from(self.repositories.as_slice()),
        );
        body.insert("permissions".to_owned(), Value::Object(permissions));
        Value::Object(body)
    }

    /// The permissions a token minted for this scope must hold exactly: the requested ones,
    /// plus `metadata: read`, which GitHub grants every token.
    fn granted(&self) -> BTreeMap<&'static str, &'static str> {
        let mut granted: BTreeMap<_, _> = self
            .permissions
            .iter()
            .map(|(permission, access)| (permission.code(), access.code()))
            .collect();
        granted
            .entry(Permission::Metadata.code())
            .or_insert(Access::Read.code());
        granted
    }
}

/// An installation token. Its `Debug` never shows the secret.
pub struct InstallationToken {
    secret: String,
    expires_at: String,
}

impl InstallationToken {
    /// The token, for the transport of the job that uses it. Never log it.
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// When GitHub says it expires, as GitHub wrote it (RFC 3339).
    #[must_use]
    pub fn expires_at(&self) -> &str {
        &self.expires_at
    }
}

impl fmt::Debug for InstallationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallationToken")
            .field("secret", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// `POST /app/installations/{id}/access_tokens`'s response. Its `Debug` never shows the token.
#[derive(Deserialize)]
struct TokenDto {
    token: String,
    expires_at: String,
    permissions: BTreeMap<String, String>,
    repository_selection: String,
    repositories: Vec<NamedRepository>,
}

impl fmt::Debug for TokenDto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenDto")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("permissions", &self.permissions)
            .field("repository_selection", &self.repository_selection)
            .field("repositories", &self.repositories)
            .finish()
    }
}

/// A repository the token covers.
#[derive(Debug, Deserialize)]
struct NamedRepository {
    name: String,
}

impl<T: Transport, S: Sleep> Api<T, S> {
    /// Mints a token of `installation` narrowed to `scope`. The transport must be
    /// authenticated with the App's JWT; minting twice is harmless, so failures are retried.
    ///
    /// # Errors
    ///
    /// [`ForgeError::ScopeMismatch`] when GitHub granted other permissions or repositories than
    /// `scope` names; [`ForgeError::Malformed`] for an unusable token; the request's errors.
    pub fn mint_installation_token(
        &self,
        installation: InstallationId,
        scope: &TokenScope,
    ) -> Result<InstallationToken, ForgeError<T::Error>> {
        let operation = Operation::MintToken;
        let body = encode(operation, &scope.body())?;
        let path = super::api_path(
            operation,
            format!("app/installations/{}/access_tokens", installation.0),
        )?;
        let response = self.send(operation, Request::post(&path, &body))?;
        let minted: TokenDto = decode(operation, &response.body)?;
        if minted.token.is_empty() || !minted.token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(ForgeError::Malformed {
                operation,
                detail: "the token is empty or not printable",
            });
        }
        let granted_repositories = lowercase_sorted(
            minted
                .repositories
                .iter()
                .map(|repository| &*repository.name),
        );
        let permissions: BTreeMap<&str, &str> = minted
            .permissions
            .iter()
            .map(|(permission, access)| (permission.as_str(), access.as_str()))
            .collect();
        if minted.repository_selection != "selected"
            || granted_repositories
                != lowercase_sorted(scope.repositories.iter().map(String::as_str))
            || permissions != scope.granted()
        {
            return Err(ForgeError::ScopeMismatch);
        }
        Ok(InstallationToken {
            secret: minted.token,
            expires_at: minted.expires_at,
        })
    }
}

/// `names` lowercased and sorted, for comparing repository lists as GitHub does, without case.
fn lowercase_sorted<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut names: Vec<String> = names.map(str::to_ascii_lowercase).collect();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_token_never_shows_in_debug_output() {
        let minted = TokenDto {
            token: "ghs_fixture-not-a-token".to_owned(),
            expires_at: "2026-10-19T04:00:00Z".to_owned(),
            permissions: BTreeMap::new(),
            repository_selection: "selected".to_owned(),
            repositories: Vec::new(),
        };

        let shown = format!("{minted:?}");

        assert!(!shown.contains("ghs_"), "{shown}");
        assert!(shown.contains("2026-10-19T04:00:00Z"), "{shown}");
    }
}
