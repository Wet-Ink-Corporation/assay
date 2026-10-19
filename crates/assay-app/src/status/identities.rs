//! The base P10 identity lists `status` reports the viewer against (#273, 03 §1): base
//! `.assay/owners` and `.assay/agents`, read through the same object port `plan` reads base
//! config through. An absent file names nobody; anything else is read exactly or refused
//! (wi-b5affa).

use std::collections::BTreeSet;

use thiserror::Error;

use super::{Identities, UserId};
use crate::config::{CommitId, IdentitiesConfigError, parse_agents, parse_owners};
use crate::plan::{EntryKind, PullRequestObjects};

/// The owners, approvers and maintainers.
const OWNERS: &str = ".assay/owners";
/// The agent identities.
const AGENTS: &str = ".assay/agents";

/// The largest list read, in bytes; a larger one is refused unread (H-13).
pub const MAX_IDENTITY_FILE_BYTES: u64 = 64 << 10;

/// Why the base identity lists could not be read. They come from the protected base, so
/// messages may name their paths.
#[derive(Debug, Error)]
pub enum BaseIdentitiesError<E> {
    /// The base could not be listed or a list read.
    #[error("could not read the base identity lists")]
    Objects(#[source] E),
    /// A list is a symlink, submodule, executable or directory.
    #[error("{path} in the base is not a regular file")]
    NotARegularFile {
        /// The list's path.
        path: &'static str,
    },
    /// A list is larger than [`MAX_IDENTITY_FILE_BYTES`].
    #[error("{path} in the base is {size} bytes, over {MAX_IDENTITY_FILE_BYTES}")]
    TooLarge {
        /// The list's path.
        path: &'static str,
        /// Its listed size.
        size: u64,
    },
    /// The object port answered a read of the list with other than one blob.
    #[error("{path} in the base was not read back")]
    Unread {
        /// The list's path.
        path: &'static str,
    },
    /// A list is not UTF-8.
    #[error("{path} in the base is not UTF-8")]
    NotUtf8 {
        /// The list's path.
        path: &'static str,
    },
    /// A list does not parse.
    #[error("{path} in the base was refused")]
    Config {
        /// The list's path.
        path: &'static str,
        /// Why.
        #[source]
        source: IdentitiesConfigError,
    },
    /// An agent identity is also privileged, which 03 §1 forbids (agents are never owners).
    #[error("id {id} is both an agent and an owner or approver")]
    AgentPrivileged {
        /// The id.
        id: u64,
    },
}

/// The base P10 identity lists at `base`: `.assay/owners`' `owners` and `approvers` as the
/// privileged ids, and `.assay/agents`' `agents`.
///
/// # Errors
///
/// [`BaseIdentitiesError`] when the base cannot be read, a list is not a regular UTF-8 file of
/// at most [`MAX_IDENTITY_FILE_BYTES`] that parses exactly, or an agent is privileged.
///
/// # Examples
///
/// ```
/// use assay_app::{CommitId, Identities, PullRequestObjects, base_identities};
/// # use assay_app::{ChangedFiles, TreeEntry};
/// # use std::convert::Infallible as Never;
/// # struct Empty;
/// # impl PullRequestObjects for Empty {
/// #     type Error = Never;
/// #     fn changed_files(&self, _: &CommitId, _: &CommitId) -> Result<ChangedFiles, Never> {
/// #         Ok(ChangedFiles::Listed(Vec::new()))
/// #     }
/// #     fn list(&self, _: &CommitId, _: Option<&str>) -> Result<Vec<TreeEntry>, Never> {
/// #         Ok(Vec::new())
/// #     }
/// #     fn read(&self, _: &[&TreeEntry]) -> Result<Vec<Vec<u8>>, Never> {
/// #         Ok(Vec::new())
/// #     }
/// # }
/// let base: CommitId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?;
/// // A base with neither list names nobody.
/// assert_eq!(base_identities(&Empty, &base)?, Identities::default());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn base_identities<O: PullRequestObjects>(
    objects: &O,
    base: &CommitId,
) -> Result<Identities, BaseIdentitiesError<O::Error>> {
    let privileged = match read_list(objects, base, OWNERS)? {
        Some(text) => parse_owners(&text).map_err(|source| BaseIdentitiesError::Config {
            path: OWNERS,
            source,
        })?,
        None => BTreeSet::new(),
    };
    let agents = match read_list(objects, base, AGENTS)? {
        Some(text) => parse_agents(&text).map_err(|source| BaseIdentitiesError::Config {
            path: AGENTS,
            source,
        })?,
        None => BTreeSet::new(),
    };
    if let Some(UserId(id)) = agents.intersection(&privileged).next() {
        return Err(BaseIdentitiesError::AgentPrivileged { id: id.get() });
    }
    Ok(Identities { privileged, agents })
}

/// The text of the list at `path` in `base`, or `None` when the base has nothing there.
fn read_list<O: PullRequestObjects>(
    objects: &O,
    base: &CommitId,
    path: &'static str,
) -> Result<Option<String>, BaseIdentitiesError<O::Error>> {
    let entries = objects
        .list(base, Some(path))
        .map_err(BaseIdentitiesError::Objects)?;
    let entry = match entries.as_slice() {
        [] => return Ok(None),
        [entry] if entry.path == path && entry.kind == EntryKind::File => entry,
        // A directory lists what is under it; a symlink or submodule is not followed.
        _ => return Err(BaseIdentitiesError::NotARegularFile { path }),
    };
    let size = entry
        .size
        .ok_or(BaseIdentitiesError::NotARegularFile { path })?;
    if size > MAX_IDENTITY_FILE_BYTES {
        return Err(BaseIdentitiesError::TooLarge { path, size });
    }
    let blobs = objects
        .read(&[entry])
        .map_err(BaseIdentitiesError::Objects)?;
    let [bytes] =
        <[Vec<u8>; 1]>::try_from(blobs).map_err(|_not_one| BaseIdentitiesError::Unread { path })?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_not_utf8| BaseIdentitiesError::NotUtf8 { path })
}
