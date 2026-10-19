//! The contents API's wire forms for a ledger file (OUT-01). Read through the duplicate-key
//! refusing parse; GitHub's other fields are ignored.

use serde::Deserialize;

/// `GET repos/{owner}/{repo}/contents/{path}` for a file.
#[derive(Debug, Deserialize)]
pub(super) struct ContentsFile {
    /// `file`, `dir`, `symlink` or `submodule`; only `file` is read.
    #[serde(rename = "type")]
    pub(super) kind: String,
    /// `base64`, or `none` for a file over 1 MB, whose content is then empty.
    pub(super) encoding: String,
    /// The file's length in bytes.
    pub(super) size: u64,
    /// The file's path in the repository.
    pub(super) path: String,
    /// The content, base64 with line breaks.
    pub(super) content: String,
    /// The git blob sha: the version a write is conditioned on.
    pub(super) sha: String,
}

/// `GET repos/{owner}/{repo}`: only whether the token sees it, and as which repository.
#[derive(Debug, Deserialize)]
pub(super) struct RepositoryView {
    /// `owner/name`, in GitHub's case.
    pub(super) full_name: String,
}

/// `GET repos/{owner}/{repo}/git/blobs/{sha}`.
#[derive(Debug, Deserialize)]
pub(super) struct Blob {
    /// The blob's sha.
    pub(super) sha: String,
    /// Its length in bytes.
    pub(super) size: u64,
    /// `base64`.
    pub(super) encoding: String,
    /// The content, base64 with line breaks.
    pub(super) content: String,
}
