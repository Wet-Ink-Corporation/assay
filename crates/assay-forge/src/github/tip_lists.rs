//! `assay-app`'s `TipLists` port (#61 DO-9, D4 A): the base tip's `.assay/owners` and
//! `.assay/agents` through the contents API at `ref=<base_ref>`, with the App's `contents: read`.
//!
//! Only a regular file at exactly the requested path is read. GitHub answers a directory with a
//! list of entries, a symlink it cannot resolve to a file of the repository with a `symlink`
//! object, and a symlink to a file of the repository with that file, under the target's path;
//! each is refused, as is a file over [`MAX_TIP_LIST_BYTES`], before anything is decoded.

use std::fmt;

use assay_app::{AGENTS_PATH, MAX_TIP_LIST_BYTES, OWNERS_PATH, TipListFiles, TipLists};
use serde::Deserialize;
use serde_json::Value;

use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};
use super::{GitHub, base64};

/// Why the base tip's eligibility lists could not be read, or were refused.
#[derive(Debug, thiserror::Error)]
pub enum TipListsError<E> {
    /// GitHub could not be reached, refused the request, or answered in a shape the adapter
    /// does not read.
    #[error(transparent)]
    Forge(#[from] ForgeError<E>),
    /// The file is over [`MAX_TIP_LIST_BYTES`]; it was not decoded.
    #[error("{path} at the base tip is {size} bytes, over the {MAX_TIP_LIST_BYTES}-byte cap")]
    TooLarge {
        /// The file.
        path: &'static str,
        /// Its length in bytes, as GitHub lists it.
        size: u64,
    },
    /// The path is not a regular file.
    #[error("{path} at the base tip is a {entry}, not a regular file")]
    NotAFile {
        /// The path.
        path: &'static str,
        /// What it is.
        entry: TipEntry,
    },
}

/// What a tip list path is when it is not a regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TipEntry {
    /// A directory.
    Directory,
    /// A symlink, whether or not GitHub followed it.
    Symlink,
    /// A submodule.
    Submodule,
}

impl fmt::Display for TipEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Submodule => "submodule",
        })
    }
}

/// `GET repos/{owner}/{repo}/contents/{path}` for a single entry; GitHub's other fields are
/// ignored.
#[derive(Debug, Deserialize)]
struct EntryDto {
    /// `file`, `dir`, `symlink` or `submodule`.
    #[serde(rename = "type")]
    kind: String,
    /// The entry's path in the repository: the target's, when GitHub followed a symlink.
    path: String,
    /// Its length in bytes.
    size: u64,
    /// `base64` for a file under 1 MB.
    encoding: Option<String>,
    /// The content, base64 with line breaks.
    content: Option<String>,
}

impl<T: Transport, S: Sleep> TipLists for GitHub<T, S> {
    type Error = TipListsError<T::Error>;

    /// A 404 is absence. It may also be a base ref GitHub does not have; absent lists narrow
    /// the recorded ones to no one, so that reading fails closed (R3-26).
    fn lists_at(&self, base_ref: &str) -> Result<TipListFiles, Self::Error> {
        Ok(TipListFiles {
            owners: self.tip_file(base_ref, OWNERS_PATH)?,
            agents: self.tip_file(base_ref, AGENTS_PATH)?,
        })
    }
}

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// The bytes of `path` at `base_ref`, or `None` when GitHub does not have it.
    fn tip_file(
        &self,
        base_ref: &str,
        path: &'static str,
    ) -> Result<Option<Vec<u8>>, TipListsError<T::Error>> {
        let operation = Operation::ReadContents;
        let malformed = |detail| ForgeError::Malformed { operation, detail };
        let target = self.contents(operation, base_ref, path)?;
        let Some(answer) = self.api.get_optional::<Value>(operation, &target)? else {
            return Ok(None);
        };
        if answer.is_array() {
            return Err(TipListsError::NotAFile {
                path,
                entry: TipEntry::Directory,
            });
        }
        let entry: EntryDto = serde_json::from_value(answer)
            .map_err(|source| ForgeError::Json { operation, source })?;
        let not_a_file = |entry| TipListsError::NotAFile { path, entry };
        match entry.kind.as_str() {
            "file" => {}
            "dir" => return Err(not_a_file(TipEntry::Directory)),
            "symlink" => return Err(not_a_file(TipEntry::Symlink)),
            "submodule" => return Err(not_a_file(TipEntry::Submodule)),
            _ => return Err(malformed("unknown contents entry type").into()),
        }
        if entry.path != path {
            return Err(not_a_file(TipEntry::Symlink));
        }
        if usize::try_from(entry.size).map_or(true, |size| size > MAX_TIP_LIST_BYTES) {
            return Err(TipListsError::TooLarge {
                path,
                size: entry.size,
            });
        }
        if entry.encoding.as_deref() != Some("base64") {
            return Err(malformed("not a base64-encoded file").into());
        }
        let bytes = entry
            .content
            .as_deref()
            .and_then(base64::decode)
            .ok_or_else(|| malformed("the content is not strict base64"))?;
        if u64::try_from(bytes.len()).ok() != Some(entry.size) {
            return Err(malformed("the content does not match its size").into());
        }
        Ok(Some(bytes))
    }
}
