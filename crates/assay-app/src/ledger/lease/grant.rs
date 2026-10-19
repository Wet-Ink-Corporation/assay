//! The one-shot retry grants, `assay.ledger-grant/1` (06 §1, R3-14): decide-only records
//! `/assay retry <role>` as an atomically created ref, and `admit` consumes it by
//! compare-and-set before applying the role's ×1.5 cap.

use std::borrow::Cow;
use std::num::NonZeroU64;

use assay_domain::RunUid;
use serde::{Deserialize, Serialize};

use super::document::{line_of, to_message};
use super::error::LeaseError;
use super::name::LeaseRef;
use super::refs::{LedgerRefs, RefCreation, RefDeletion, RefTip};
use crate::config::CommitId;
use crate::ledger::lines::{Newline, check_canonical, check_schema, parse};
use crate::ledger::record::{field, is_repository};
use crate::ledger::{Corruption, LedgerTime};
use crate::prompt_bundle::Role;

/// The contract id every grant carries.
const GRANT_SCHEMA: &str = "assay.ledger-grant/1";

/// A retry grant: `{run_uid, repo, pr, head_sha, role, at}`. Its ref is named by the grant's
/// repository (`run_uid.repo_id`, decide-only runs in the PR's repository), PR, head and role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryGrant {
    /// The decide-only run that granted it.
    pub run_uid: RunUid,
    /// The PR's repository, `owner/name`.
    pub repo: String,
    /// The PR.
    pub pr: NonZeroU64,
    /// The head the retry is for.
    pub head_sha: CommitId,
    /// The role whose cap the retry raises.
    pub role: Role,
    /// When it was granted.
    pub at: LedgerTime,
}

/// Which grant `admit` looks for: its ref's parts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GrantKey {
    /// The PR's repository id.
    pub repo_id: NonZeroU64,
    /// The PR.
    pub pr: NonZeroU64,
    /// The head reviewed.
    pub head_sha: CommitId,
    /// The role.
    pub role: Role,
}

/// A grant's ref parts, borrowed from a [`GrantKey`] or a [`RetryGrant`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct GrantKeyRef<'a> {
    repo_id: NonZeroU64,
    pr: NonZeroU64,
    head_sha: &'a CommitId,
    role: Role,
}

impl<'a> GrantKeyRef<'a> {
    const fn of_key(key: &'a GrantKey) -> Self {
        Self {
            repo_id: key.repo_id,
            pr: key.pr,
            head_sha: &key.head_sha,
            role: key.role,
        }
    }

    /// The ref `grant` belongs at: decide-only runs in the PR's repository, so the run's
    /// repository id is the grant's.
    const fn of_grant(grant: &'a RetryGrant) -> Self {
        Self {
            repo_id: grant.run_uid.repo_id,
            pr: grant.pr,
            head_sha: &grant.head_sha,
            role: grant.role,
        }
    }

    fn name(self) -> LeaseRef {
        LeaseRef::retry_grant(self.repo_id, self.pr, self.head_sha, self.role)
    }
}

/// What granting did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Granted {
    /// The grant was created.
    Granted,
    /// A grant for this head and role was already waiting; nothing was written.
    AlreadyGranted,
}

/// What consuming found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consumed {
    /// This call consumed the grant: the retry's ×1.5 cap applies to its role.
    Consumed(RetryGrant),
    /// There was no grant, or another call consumed it first.
    NoGrant,
}

/// Records `grant` by creating its ref; a second grant for the same head and role is not
/// created while the first waits.
///
/// # Errors
///
/// [`LeaseError::Invalid`] when the grant would break its contract, and [`LeaseError::Refs`] when
/// the ref cannot be created.
pub fn grant_retry<R: LedgerRefs>(
    refs: &R,
    grant: &RetryGrant,
) -> Result<Granted, LeaseError<R::Error>> {
    let key = GrantKeyRef::of_grant(grant);
    let message = to_message(&GrantLine::from(grant))
        .and_then(|message| read_grant(&message, key).map(|_grant| message))
        .map_err(|problem| LeaseError::Invalid { problem })?;
    Ok(
        match refs
            .create(&key.name(), &message)
            .map_err(LeaseError::Refs)?
        {
            RefCreation::Created(_commit) => Granted::Granted,
            RefCreation::Exists => Granted::AlreadyGranted,
        },
    )
}

/// Consumes the grant `key` names, by deleting its ref on condition that it still points at
/// the commit read: of two consumers exactly one gets [`Consumed::Consumed`].
///
/// # Errors
///
/// [`LeaseError::Corrupt`] when the grant cannot be read or names another ref (it is not
/// consumed), and [`LeaseError::Refs`] when the ref cannot be read or deleted.
pub fn consume_grant<R: LedgerRefs>(
    refs: &R,
    key: &GrantKey,
) -> Result<Consumed, LeaseError<R::Error>> {
    let key = GrantKeyRef::of_key(key);
    let name = key.name();
    let Some(RefTip { commit, message }) = refs.read(&name).map_err(LeaseError::Refs)? else {
        return Ok(Consumed::NoGrant);
    };
    let grant = read_grant(&message, key).map_err(|problem| LeaseError::Corrupt {
        name: name.clone(),
        problem,
    })?;
    Ok(
        match refs.delete_if(&name, &commit).map_err(LeaseError::Refs)? {
            RefDeletion::Deleted => Consumed::Consumed(grant),
            RefDeletion::Moved => Consumed::NoGrant,
        },
    )
}

/// The grant `key` names, when one waits, read without consuming it: what `admit` reserves for
/// before it takes a slot, so a run that is then deferred leaves the grant waiting.
///
/// # Errors
///
/// [`LeaseError::Corrupt`] when the grant cannot be read or names another ref, and
/// [`LeaseError::Refs`] when the ref cannot be read.
pub fn pending_grant<R: LedgerRefs>(
    refs: &R,
    key: &GrantKey,
) -> Result<Option<RetryGrant>, LeaseError<R::Error>> {
    let key = GrantKeyRef::of_key(key);
    let name = key.name();
    let Some(RefTip { message, .. }) = refs.read(&name).map_err(LeaseError::Refs)? else {
        return Ok(None);
    };
    read_grant(&message, key)
        .map(Some)
        .map_err(|problem| LeaseError::Corrupt { name, problem })
}

/// The grant a commit message carries, exactly as assay writes it, refused unless its parts are
/// those of `key`.
fn read_grant(message: &str, key: GrantKeyRef<'_>) -> Result<RetryGrant, Corruption> {
    let line = line_of(message)?;
    let grant = RetryGrant::try_from(parse::<GrantLine<'_>>(line)?)?;
    check_canonical(line, &GrantLine::from(&grant), Newline::Stripped)?;
    if GrantKeyRef::of_grant(&grant) != key {
        return Err(Corruption::WrongRef);
    }
    Ok(grant)
}

/// The wire form of a grant (OUT-01).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantLine<'a> {
    schema: Cow<'static, str>,
    run_uid: RunUid,
    repo: Cow<'a, str>,
    pr: NonZeroU64,
    head_sha: String,
    role: Cow<'static, str>,
    at: String,
}

impl<'a> From<&'a RetryGrant> for GrantLine<'a> {
    fn from(grant: &'a RetryGrant) -> Self {
        Self {
            schema: Cow::Borrowed(GRANT_SCHEMA),
            run_uid: grant.run_uid,
            repo: Cow::Borrowed(&grant.repo),
            pr: grant.pr,
            head_sha: grant.head_sha.to_string(),
            role: Cow::Borrowed(grant.role.code()),
            at: grant.at.to_string(),
        }
    }
}

impl TryFrom<GrantLine<'_>> for RetryGrant {
    type Error = Corruption;

    fn try_from(line: GrantLine<'_>) -> Result<Self, Corruption> {
        check_schema(&line.schema, GRANT_SCHEMA)?;
        if !is_repository(&line.repo) {
            return Err(Corruption::Field("repo"));
        }
        let role = Role::ALL
            .into_iter()
            .find(|role| role.code() == line.role)
            .ok_or(Corruption::Field("role"))?;
        Ok(Self {
            run_uid: line.run_uid,
            repo: line.repo.into_owned(),
            pr: line.pr,
            head_sha: field(&line.head_sha, "head_sha")?,
            role,
            at: field(&line.at, "at")?,
        })
    }
}
