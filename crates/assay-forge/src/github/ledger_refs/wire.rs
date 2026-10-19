//! The git data API's wire forms for a lease ref and its commit (OUT-01). Read through the
//! duplicate-key refusing parse; GitHub's other fields are ignored.

use serde::Deserialize;
use serde::de::IgnoredAny;

/// `GET repos/{owner}/{repo}/git/ref/{ref}` and `POST repos/{owner}/{repo}/git/refs`.
#[derive(Debug, Deserialize)]
pub(super) struct RefView {
    /// The full ref name.
    #[serde(rename = "ref")]
    pub(super) name: String,
    /// What it points at.
    pub(super) object: RefObject,
}

/// The object a ref points at.
#[derive(Debug, Deserialize)]
pub(super) struct RefObject {
    /// Its id.
    pub(super) sha: String,
    /// `commit`, `tag`, `tree` or `blob`; only `commit` is read.
    #[serde(rename = "type")]
    pub(super) kind: String,
}

/// `GET repos/{owner}/{repo}/git/commits/{sha}` and `POST repos/{owner}/{repo}/git/commits`.
#[derive(Debug, Deserialize)]
pub(super) struct CommitView {
    /// Its id.
    pub(super) sha: String,
    /// The whole message.
    pub(super) message: String,
    /// Its tree.
    pub(super) tree: Tree,
    /// Its parents; a lease commit has none.
    pub(super) parents: Vec<IgnoredAny>,
}

/// A commit's tree.
#[derive(Debug, Deserialize)]
pub(super) struct Tree {
    /// The tree's id.
    pub(super) sha: String,
}
