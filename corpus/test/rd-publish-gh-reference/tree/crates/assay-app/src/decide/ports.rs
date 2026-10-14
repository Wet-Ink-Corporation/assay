//! The ports `decide` reads GitHub through: the PR's live state and the run's artifacts.

use std::num::NonZeroU64;

use assay_domain::RunUid;

use crate::config::CommitId;

/// GitHub's view of the PR at decide time, read with the App token. Implemented by the forge
/// adapter.
pub trait PullRequestState {
    /// Why GitHub could not be read.
    type Error: std::error::Error;

    /// The PR's current head commit and base branch (`GET /pulls/{number}`).
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the PR cannot be read.
    fn current(&self, number: NonZeroU64) -> Result<CurrentPullRequest, Self::Error>;

    /// Every PR whose head is `head`, open or not (`GET /commits/{sha}/pulls`), as far as the
    /// adapter pages.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the list cannot be read.
    fn pulls_on_head(&self, head: &CommitId) -> Result<Vec<PullOnHead>, Self::Error>;

    /// Whether a run of this workflow newer than `run` exists for `head`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the runs cannot be listed.
    fn newer_run(&self, run: RunUid, head: &CommitId) -> Result<bool, Self::Error>;
}

/// A PR's head and base as GitHub reports them now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentPullRequest {
    /// The head commit.
    pub head_sha: CommitId,
    /// The base branch (`pull.base.ref`).
    pub base_ref: String,
}

/// One PR on a head commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOnHead {
    /// Its number.
    pub number: NonZeroU64,
    /// Whether it is open.
    pub open: bool,
    /// The base branch it targets.
    pub base_ref: String,
}

/// The run's workflow artifacts, read with the run's token. Implemented by the forge adapter.
pub trait RunArtifacts {
    /// Why the artifacts could not be read.
    type Error: std::error::Error;

    /// The bytes of `file` in the artifact named `artifact`, read under `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the artifact store cannot be read; an absent artifact or file is
    /// [`Fetched::Missing`] instead.
    fn fetch(&self, artifact: &str, file: &str, limit: usize) -> Result<Fetched, Self::Error>;
}

/// What a fetch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// The file's bytes, not yet authenticated.
    Found(Vec<u8>),
    /// No such artifact or file (expired, or deleted by a writer).
    Missing,
    /// The file is larger than the limit and was not read.
    TooLarge,
}
