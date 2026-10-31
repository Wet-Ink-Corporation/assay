//! The base P10 identity lists `status` reports the viewer against (#273, 03 §1): base
//! `.assay/owners` and `.assay/agents`, read through the same object port `plan` reads base
//! config through, and parsed by [`EligibilityLists::parse`], the one parser of those files
//! (#305), so status and eligibility cannot read them differently. An absent file names nobody;
//! anything else is read exactly or refused (wi-b5affa).

use thiserror::Error;

use super::Identities;
use crate::config::{CommitId, EligibilityLists, ListsError, MAX_LISTS_BYTES};
use crate::plan::{EntryKind, PullRequestObjects, TreeEntry};

/// The base config directory, which holds the lists.
const CONFIG_DIR: &str = ".assay";
/// The owners, approvers and maintainers.
const OWNERS: &str = ".assay/owners";
/// The agent identities.
const AGENTS: &str = ".assay/agents";

/// Why the base identity lists could not be read. They come from the protected base, so
/// messages may name their paths.
#[derive(Debug, Error)]
pub enum BaseIdentitiesError<E> {
    /// The base could not be listed or a list read.
    #[error("could not read the base identity lists")]
    Objects(#[source] E),
    /// `.assay` is a symlink or submodule, which is never followed.
    #[error("{path} in the base is not a directory")]
    NotADirectory {
        /// The directory's path.
        path: &'static str,
    },
    /// A list is a symlink, submodule or directory (an executable file is read like any
    /// other).
    #[error("{path} in the base is not a regular file")]
    NotARegularFile {
        /// The list's path.
        path: &'static str,
    },
    /// A list is larger than [`MAX_LISTS_BYTES`]; it was not read.
    #[error("{path} in the base is {size} bytes, over {MAX_LISTS_BYTES}")]
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
    /// The lists were refused by [`EligibilityLists::parse`], which names the file or list.
    #[error("the base identity lists were refused")]
    Lists(#[source] ListsError),
}

/// The base P10 identity lists at `base`: `.assay/owners`' `owners` and `approvers` as the
/// privileged ids, and `.assay/agents`' `agents`.
///
/// # Errors
///
/// [`BaseIdentitiesError`] when the base cannot be read, `.assay` is a symlink or submodule, a
/// list is not a regular UTF-8 file of at most [`MAX_LISTS_BYTES`], or
/// [`EligibilityLists::parse`] refuses the lists.
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
    // `.assay` itself first, as `plan` reads base config: a symlink or submodule there lists
    // as one entry, and the lists under it must not read as absent.
    let entries = objects
        .list(base, Some(CONFIG_DIR))
        .map_err(BaseIdentitiesError::Objects)?;
    if entries.iter().any(|entry| entry.path == CONFIG_DIR) {
        return Err(BaseIdentitiesError::NotADirectory { path: CONFIG_DIR });
    }
    let owners = read_list(objects, &entries, OWNERS)?;
    let agents = read_list(objects, &entries, AGENTS)?;
    let lists = EligibilityLists::parse(owners.as_deref(), agents.as_deref())
        .map_err(BaseIdentitiesError::Lists)?;
    Ok(Identities {
        privileged: lists
            .owners()
            .iter()
            .chain(lists.approvers())
            .map(|identity| identity.id)
            .collect(),
        agents: lists.agents().iter().map(|identity| identity.id).collect(),
    })
}

/// The text of the list at `path` among the base's `.assay` `entries`, or `None` when the base
/// has nothing there.
fn read_list<O: PullRequestObjects>(
    objects: &O,
    entries: &[TreeEntry],
    path: &'static str,
) -> Result<Option<String>, BaseIdentitiesError<O::Error>> {
    let at_or_under: Vec<&TreeEntry> = entries
        .iter()
        .filter(|entry| {
            entry
                .path
                .strip_prefix(path)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .collect();
    let entry = match at_or_under.as_slice() {
        [] => return Ok(None),
        [entry] if entry.path == path && entry.kind == EntryKind::File => *entry,
        // A directory lists what is under it; a symlink or submodule is not followed.
        _ => return Err(BaseIdentitiesError::NotARegularFile { path }),
    };
    let size = entry
        .size
        .ok_or(BaseIdentitiesError::NotARegularFile { path })?;
    if usize::try_from(size).map_or(true, |size| size > MAX_LISTS_BYTES) {
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
