//! The ledger repository's lease refs: `assay-app`'s [`LedgerRefs`] over the REST git data API
//! for creation and reads, and over the hardened git layer's push for deletion (06 §1, A-26,
//! A-33), with the `assay-ledger-writer` App's installation token (ADR-26).
//!
//! A ref is created at a new parentless commit of the empty tree whose message is the lease
//! (`POST git/commits`, then `POST git/refs`, which GitHub refuses with 422 when the ref exists),
//! read with `GET git/ref/<name>` and `GET git/commits/<sha>`, and deleted only by `git push
//! --force-with-lease=<ref>:<commit> <ledger> :<ref>`: REST `DELETE /git/refs` has no
//! precondition and is never sent. Everything about the leases is `assay-app`'s.

mod wire;

use assay_app::{
    CommitId, LeaseRef, LedgerRefs, MAX_LEASE_MESSAGE_BYTES, RefCreation, RefDeletion, RefTip,
};
use serde_json::json;

use super::api::{Api, Failed, decode, encode};
use super::api_path;
use super::error::{ForgeError, Operation};
use super::ledger::{confirm_repository, encode_base64};
use super::repository::Repository;
use super::token::InstallationToken;
use super::transport::{ApiPath, Request, Sleep, Transport};
use crate::git::{
    Authorization, BranchRef, CommitSha, DeleteOutcome, GitError, HttpsRemote, InvalidHeader,
    ObjectRepo, ParseRemoteError, PushRemote,
};
use wire::{CommitView, RefView};

/// The empty tree, which every lease commit carries.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The ledger repository's lease refs.
#[derive(Debug)]
pub struct LedgerRefsRepo<T, S> {
    api: Api<T, S>,
    repository: Repository,
    pusher: ObjectRepo,
    remote: PushRemote,
}

/// Why a lease ref could not be read, created or deleted.
#[derive(Debug, thiserror::Error)]
pub enum LedgerRefsError<E> {
    /// GitHub could not be reached, refused the request, or answered in a shape the adapter
    /// does not read.
    #[error(transparent)]
    Forge(#[from] ForgeError<E>),
    /// The deletion's push failed or could not be run; the ref may or may not have been deleted
    /// when the push did not run to completion.
    #[error("the ledger ref could not be deleted")]
    Git(#[from] GitError),
    /// The lease message is over [`MAX_LEASE_MESSAGE_BYTES`]: not sent, or not read.
    #[error(
        "the lease message of {name} is {size} bytes, over the {MAX_LEASE_MESSAGE_BYTES}-byte cap"
    )]
    TooLong {
        /// The ref.
        name: LeaseRef,
        /// The message's length in bytes.
        size: usize,
    },
    /// The ref or commit cannot be named to git; nothing was pushed.
    #[error("{name} cannot be pushed")]
    Unpushable {
        /// The ref.
        name: LeaseRef,
    },
}

/// Why the ledger's push remote could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LedgerRemoteError {
    /// The repository's URL is not an https remote.
    #[error(transparent)]
    Remote(#[from] ParseRemoteError),
    /// The token cannot be sent in a header.
    #[error(transparent)]
    Header(#[from] InvalidHeader),
}

/// The push remote of the ledger repository `repository`.
///
/// It is `https://github.com/<owner>/<name>.git`, authenticated as the installation `token`
/// belongs to (`x-access-token` basic credentials, sent to that URL only).
///
/// # Errors
///
/// [`LedgerRemoteError`] when the URL or the header cannot be formed (a token holding a control
/// character).
pub fn ledger_push_remote(
    repository: &Repository,
    token: &InstallationToken,
) -> Result<PushRemote, LedgerRemoteError> {
    let remote: HttpsRemote = format!("https://github.com/{repository}.git").parse()?;
    let credentials = encode_base64(format!("x-access-token:{}", token.secret()).as_bytes());
    let authorization = Authorization::new(format!("Authorization: basic {credentials}"))?;
    Ok(PushRemote::https(remote, authorization))
}

/// How the ref creation's attempts ended.
enum Creation {
    Created,
    Exists,
}

impl<T: Transport, S: Sleep> LedgerRefsRepo<T, S> {
    /// The lease refs of the ledger repository `repository`, created and read through `api` and
    /// deleted by pushes from the object-only repository `pusher` to `remote` (built with
    /// [`ledger_push_remote`]); both must carry an `assay-ledger-writer` installation token
    /// (`contents: write`).
    pub const fn new(
        api: Api<T, S>,
        repository: Repository,
        pusher: ObjectRepo,
        remote: PushRemote,
    ) -> Self {
        Self {
            api,
            repository,
            pusher,
            remote,
        }
    }

    /// `repos/<owner>/<name>/<rest>`, for `operation`.
    fn repo_path<E>(&self, operation: Operation, rest: &str) -> Result<ApiPath, ForgeError<E>> {
        api_path(operation, format!("repos/{}/{rest}", self.repository))
    }

    /// The commit `name` points at, refused unless GitHub answered for that ref and a commit.
    fn tip(
        &self,
        operation: Operation,
        name: &LeaseRef,
    ) -> Result<Option<CommitId>, ForgeError<T::Error>> {
        let malformed = |detail| ForgeError::Malformed { operation, detail };
        let short = name
            .as_str()
            .strip_prefix("refs/")
            .ok_or_else(|| malformed("a lease ref is under refs/"))?;
        let path = self.repo_path(operation, &format!("git/ref/{short}"))?;
        let Some(found) = self.api.get_optional::<RefView>(operation, &path)? else {
            return Ok(None);
        };
        if found.name != name.as_str() {
            return Err(malformed("the API answered for another ref"));
        }
        if found.object.kind != "commit" {
            return Err(malformed("the lease ref does not point at a commit"));
        }
        let commit = found
            .object
            .sha
            .parse()
            .map_err(|_short| malformed("the ref's commit is not a full id"))?;
        Ok(Some(commit))
    }

    /// Creates a parentless commit of the empty tree with `message`, checked as GitHub wrote it.
    fn create_commit(&self, message: &str) -> Result<CommitId, ForgeError<T::Error>> {
        let operation = Operation::CreateLedgerRef;
        let malformed = |detail| ForgeError::Malformed { operation, detail };
        let body = encode(
            operation,
            &json!({"message": message, "tree": EMPTY_TREE, "parents": []}),
        )?;
        let path = self.repo_path(operation, "git/commits")?;
        // Repeating it is harmless: a commit no ref points at is never read.
        let response = self.api.send(operation, Request::post(&path, &body))?;
        let commit: CommitView = decode(operation, &response.body)?;
        // GitHub may end a message with a newline; the lease reader accepts one.
        let written = commit.message.strip_suffix('\n').unwrap_or(&commit.message);
        if written != message || commit.tree.sha != EMPTY_TREE || !commit.parents.is_empty() {
            return Err(malformed("the lease commit is not the one asked for"));
        }
        commit
            .sha
            .parse()
            .map_err(|_short| malformed("the lease commit's id is not a full id"))
    }

    /// Creates `name` at `commit`. A 422 or an unanswered attempt is resolved by reading the ref:
    /// at `commit`, this call created it; at another commit, it exists; absent after a 422,
    /// GitHub refused the creation.
    fn create_ref(
        &self,
        name: &LeaseRef,
        commit: &CommitId,
    ) -> Result<Creation, ForgeError<T::Error>> {
        let operation = Operation::CreateLedgerRef;
        let body = encode(
            operation,
            &json!({"ref": name.as_str(), "sha": commit.as_str()}),
        )?;
        let path = self.repo_path(operation, "git/refs")?;
        let request = Request::post(&path, &body);
        let resolve = |found: Option<CommitId>| match found {
            Some(at) if at == *commit => Some(Creation::Created),
            Some(_other) => Some(Creation::Exists),
            None => None,
        };
        self.api.retrying(operation, |attempt| {
            if attempt > 0
                && let Some(settled) = resolve(self.tip(operation, name)?)
            {
                return Ok(settled);
            }
            match self.api.attempt(operation, &request) {
                Ok(_created) => Ok(Creation::Created),
                Err(Failed::Stop(refused @ ForgeError::Rejected { status: 422, .. })) => {
                    resolve(self.tip(operation, name)?).ok_or(Failed::Stop(refused))
                }
                Err(failed) => Err(failed),
            }
        })
    }

    /// Refuses a message over [`MAX_LEASE_MESSAGE_BYTES`].
    fn check_length(name: &LeaseRef, message: &str) -> Result<(), LedgerRefsError<T::Error>> {
        if message.len() > MAX_LEASE_MESSAGE_BYTES {
            return Err(LedgerRefsError::TooLong {
                name: name.clone(),
                size: message.len(),
            });
        }
        Ok(())
    }
}

impl<T: Transport, S: Sleep> LedgerRefs for LedgerRefsRepo<T, S> {
    type Error = LedgerRefsError<T::Error>;

    fn create(&self, name: &LeaseRef, message: &str) -> Result<RefCreation, Self::Error> {
        Self::check_length(name, message)?;
        let commit = self.create_commit(message)?;
        Ok(match self.create_ref(name, &commit)? {
            Creation::Created => RefCreation::Created(commit),
            Creation::Exists => RefCreation::Exists,
        })
    }

    /// A 404 for the ref counts as absent only once the repository is seen, so a token without
    /// access never reads as a free slot.
    fn read(&self, name: &LeaseRef) -> Result<Option<RefTip>, Self::Error> {
        let operation = Operation::ReadLedgerRef;
        let Some(commit) = self.tip(operation, name)? else {
            confirm_repository(&self.api, &self.repository, operation)?;
            return Ok(None);
        };
        let path = self.repo_path(operation, &format!("git/commits/{commit}"))?;
        let found: CommitView = self.api.get(operation, &path)?;
        if found.sha != commit.as_str() {
            return Err(ForgeError::Malformed {
                operation,
                detail: "the API answered for another commit",
            }
            .into());
        }
        Self::check_length(name, &found.message)?;
        Ok(Some(RefTip {
            commit,
            message: found.message,
        }))
    }

    fn delete_if(&self, name: &LeaseRef, expected: &CommitId) -> Result<RefDeletion, Self::Error> {
        let unpushable = || LedgerRefsError::Unpushable { name: name.clone() };
        let branch: BranchRef = name.as_str().parse().map_err(|_refused| unpushable())?;
        let expected: CommitSha = expected.as_str().parse().map_err(|_refused| unpushable())?;
        Ok(
            match self
                .pusher
                .delete_ref_if(&self.remote, &branch, &expected)?
            {
                DeleteOutcome::Deleted => RefDeletion::Deleted,
                DeleteOutcome::Stale => RefDeletion::Moved,
            },
        )
    }
}
