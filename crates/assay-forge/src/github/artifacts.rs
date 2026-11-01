//! The run bundles' artifact store (#273, wi-6b26d6): `assay-app`'s [`BundleSource`] and
//! [`DecisionBundleSource`] over the `gh` transport.
//!
//! The run's artifacts are listed through the API, and exactly one may be named like the
//! bundle's: the pipeline decide job's (`assay-decide*`), or decide-only's for the PR asked for
//! (`assay-decision-<pr>-*`, D5; another PR's is not counted), each compared without ASCII
//! case. It must be exactly `assay-decide-<nonce>` or `assay-decision-<pr>-<nonce>` and listed
//! for the pointer's run of this repository. It is downloaded with `gh run download`, which
//! extracts it into a scratch directory and is killed once that directory holds more than the
//! limit (an extraction past it is [`StoredBundle::TooLarge`]), and the one file in it is read
//! under the limit. The sha256 against the pointer, the run inside the bundle and its shape are
//! `assay-app`'s checks; this adapter refuses anything ambiguous before them (wi-b5affa).

use std::fs::{self, File};
use std::io::{self, Read};
use std::num::NonZeroU64;
use std::path::Path;

use assay_app::{ArtifactNonce, BundleSource, DECISION_FILE, DecisionBundleSource, StoredBundle};
use assay_domain::RunUid;
use serde::Deserialize;

use super::GitHub;
use super::api::{PER_PAGE, Page};
use super::error::{ForgeError, Operation};
use super::gh::{Downloaded, GhApi, GhDownloadError, GhError, GhRunner, Wanted};
use super::transport::Sleep;
use crate::process::ProcessError;

/// What the decide job's artifact name starts with (`plan` names it `assay-decide-<nonce>`);
/// matched without ASCII case and without the `-`, so a look-alike is counted, not skipped.
const DECIDE_PREFIX: &str = "assay-decide";

/// What decide-only's artifact names start with, before the PR number (D5).
const DECISION_PREFIX: &str = "assay-decision-";

/// The most a zip may add to the file it holds: one local header, one central directory entry
/// and the end record, with room to spare. A listed artifact larger than the limit and this is
/// refused before it is downloaded.
const MAX_ZIP_OVERHEAD: u64 = 1 << 20;

/// Why a run bundle could not be read from the artifact store. An absent, expired or oversized
/// one is a [`StoredBundle`] instead.
#[derive(Debug, thiserror::Error)]
pub enum BundleFetchError {
    /// The run's artifacts could not be listed.
    #[error("could not list the run's artifacts")]
    List(#[source] ForgeError<GhError>),
    /// More than one of the run's artifacts is named like the bundle's: which one holds it is
    /// not guessed.
    #[error("{found} artifacts of the run are named like the bundle's")]
    Ambiguous {
        /// How many.
        found: usize,
    },
    /// The one artifact named like the bundle's is not exactly its name with a nonce.
    #[error("the run's bundle artifact is not named with a nonce")]
    Name,
    /// The listing gives the artifact another run or repository than the pointer's, or none.
    #[error("the bundle artifact is listed for another run or repository")]
    OtherRun,
    /// gh did not download the artifact.
    #[error("could not download the bundle artifact")]
    Download(#[source] GhDownloadError),
    /// The download holds something other than the one regular file asked for: another entry
    /// beside it, or a symlink or directory in its place.
    #[error("the bundle artifact holds something other than the one bundle file")]
    Contents,
    /// The downloaded file could not be read.
    #[error("could not read the downloaded bundle")]
    Read(#[source] io::Error),
    /// The download's scratch directory could not be removed (the transport's `close` removes
    /// what is left).
    #[error("could not remove the download's scratch directory")]
    Cleanup(#[source] io::Error),
}

/// A page of `GET repos/<owner>/<name>/actions/runs/<id>/artifacts`.
#[derive(Debug, Deserialize)]
struct ArtifactsPage {
    artifacts: Vec<ArtifactDto>,
}

impl Page for ArtifactsPage {
    type Item = ArtifactDto;

    fn items(self) -> Vec<ArtifactDto> {
        self.artifacts
    }
}

/// One artifact as GitHub lists it.
#[derive(Debug, Deserialize)]
struct ArtifactDto {
    name: String,
    size_in_bytes: u64,
    expired: bool,
    workflow_run: Option<WorkflowRunDto>,
}

/// The run an artifact belongs to.
#[derive(Debug, Deserialize)]
struct WorkflowRunDto {
    id: u64,
    repository_id: u64,
}

/// Which of a run's artifacts holds the bundle, and its file: the exact name before the nonce
/// (`exact`), and how much of it a name must start with, compared without ASCII case, to count
/// as a candidate (`counted` bytes).
struct Selector<'a> {
    exact: String,
    counted: usize,
    file: &'a str,
}

impl<'a> Selector<'a> {
    /// `file` of the pipeline decide job's artifact, `assay-decide-<nonce>`; any
    /// `assay-decide*` counts.
    fn decide(file: &'a str) -> Self {
        Self {
            exact: format!("{DECIDE_PREFIX}-"),
            counted: DECIDE_PREFIX.len(),
            file,
        }
    }

    /// decide-only's artifact for PR `pr`, `assay-decision-<pr>-<nonce>`, holding
    /// [`DECISION_FILE`]; any `assay-decision-<pr>-*` counts, and another PR's does not.
    fn decision(pr: NonZeroU64) -> Self {
        let exact = format!("{DECISION_PREFIX}{pr}-");
        Self {
            counted: exact.len(),
            exact,
            file: DECISION_FILE,
        }
    }

    /// Whether `name` is counted as the bundle's artifact.
    fn counts(&self, name: &str) -> bool {
        let counted = self.exact.get(..self.counted);
        name.get(..self.counted)
            .zip(counted)
            .is_some_and(|(start, counted)| start.eq_ignore_ascii_case(counted))
    }

    /// Whether `name` is exactly the bundle's artifact name with a nonce.
    fn names(&self, name: &str) -> bool {
        name.strip_prefix(self.exact.as_str())
            .is_some_and(|nonce| nonce.parse::<ArtifactNonce>().is_ok())
    }
}

impl<R: GhRunner, S: Sleep> BundleSource for GitHub<GhApi<R>, S> {
    type Error = BundleFetchError;

    /// `file` of the decide job's artifact of `run`, read under `limit` bytes. The attempt is
    /// not listed with artifacts; the app checks it against the bundle's own `run_uid`.
    ///
    /// # Errors
    ///
    /// [`BundleFetchError`] when the artifacts cannot be listed, the decide artifact is
    /// ambiguous, misnamed or listed for another run, or its download fails, holds anything
    /// but `file` or cannot be read or removed.
    fn fetch(
        &self,
        run: RunUid,
        file: &str,
        limit: usize,
    ) -> Result<StoredBundle, BundleFetchError> {
        self.fetch_selected(run, &Selector::decide(file), limit)
    }
}

impl<R: GhRunner, S: Sleep> DecisionBundleSource for GitHub<GhApi<R>, S> {
    type Error = BundleFetchError;

    /// [`DECISION_FILE`] of decide-only's artifact for `pr` of `run`, read under `limit` bytes;
    /// the run's artifacts for other PRs are not counted.
    ///
    /// # Errors
    ///
    /// As [`BundleSource::fetch`], for the artifact named `assay-decision-<pr>-<nonce>`.
    fn fetch_decision(
        &self,
        run: RunUid,
        pr: NonZeroU64,
        limit: usize,
    ) -> Result<StoredBundle, BundleFetchError> {
        self.fetch_selected(run, &Selector::decision(pr), limit)
    }
}

impl<R: GhRunner, S: Sleep> GitHub<GhApi<R>, S> {
    /// The selector's file of the one artifact of `run` it selects, read under `limit` bytes.
    fn fetch_selected(
        &self,
        run: RunUid,
        selector: &Selector<'_>,
        limit: usize,
    ) -> Result<StoredBundle, BundleFetchError> {
        let operation = Operation::ListArtifacts;
        let path = self
            .path(
                operation,
                &format!("actions/runs/{}/artifacts?per_page={PER_PAGE}", run.run_id),
            )
            .map_err(BundleFetchError::List)?;
        let listed = self
            .api
            .list::<ArtifactsPage>(operation, path, None)
            .map_err(BundleFetchError::List)?;
        let artifact = match selected(listed, selector, run)? {
            Selected::Download(artifact) => artifact,
            Selected::Stored(stored) => return Ok(stored),
        };
        let zip_room = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .saturating_add(MAX_ZIP_OVERHEAD);
        if artifact.size_in_bytes > zip_room {
            return Ok(StoredBundle::TooLarge);
        }
        // The one file may hold `limit` bytes: an extraction past that is killed (the zip is
        // GitHub's listing's size, but what it unpacks to is the artifact's own claim).
        let wanted = Wanted {
            run_id: run.run_id,
            name: &artifact.name,
            max_bytes: u64::try_from(limit).unwrap_or(u64::MAX),
        };
        let downloaded = match self.api.transport().download(&self.repository, wanted) {
            Ok(downloaded) => downloaded,
            Err(GhDownloadError::Run(ProcessError::DirectoryTooLarge)) => {
                return Ok(StoredBundle::TooLarge);
            }
            Err(error) => return Err(BundleFetchError::Download(error)),
        };
        let read = read_download(downloaded.path(), selector.file, limit);
        close(downloaded, read)
    }
}

/// What the listing says to do.
enum Selected {
    /// Download this artifact.
    Download(ArtifactDto),
    /// Nothing to download: the store's answer.
    Stored(StoredBundle),
}

/// The one artifact of `listed` that `selector` counts, checked against `run`.
fn selected(
    listed: Vec<ArtifactDto>,
    selector: &Selector<'_>,
    run: RunUid,
) -> Result<Selected, BundleFetchError> {
    let mut candidates = listed
        .into_iter()
        .filter(|artifact| selector.counts(&artifact.name));
    let Some(artifact) = candidates.next() else {
        return Ok(Selected::Stored(StoredBundle::Missing));
    };
    let others = candidates.count();
    if others > 0 {
        return Err(BundleFetchError::Ambiguous {
            found: others.saturating_add(1),
        });
    }
    if !selector.names(&artifact.name) {
        return Err(BundleFetchError::Name);
    }
    let ours = artifact.workflow_run.as_ref().is_some_and(|listed| {
        listed.id == run.run_id.get() && listed.repository_id == run.repo_id.get()
    });
    if !ours {
        return Err(BundleFetchError::OtherRun);
    }
    if artifact.expired {
        return Ok(Selected::Stored(StoredBundle::Expired));
    }
    Ok(Selected::Download(artifact))
}

/// `file` in the download directory `dir`, which must hold it alone, as a regular file, read
/// under `limit` bytes. An empty directory is [`StoredBundle::Missing`].
fn read_download(dir: &Path, file: &str, limit: usize) -> Result<StoredBundle, BundleFetchError> {
    let mut entries = fs::read_dir(dir).map_err(BundleFetchError::Read)?;
    let Some(entry) = entries.next() else {
        return Ok(StoredBundle::Missing);
    };
    let entry = entry.map_err(BundleFetchError::Read)?;
    if entries.next().is_some() || entry.file_name() != file {
        return Err(BundleFetchError::Contents);
    }
    // The entry's own type: a symlink is not followed.
    let kind = entry.file_type().map_err(BundleFetchError::Read)?;
    if !kind.is_file() {
        return Err(BundleFetchError::Contents);
    }
    let room = u64::try_from(limit).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    File::open(entry.path())
        .and_then(|opened| opened.take(room.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(BundleFetchError::Read)?;
    if bytes.len() > limit {
        return Ok(StoredBundle::TooLarge);
    }
    Ok(StoredBundle::Found(bytes))
}

/// Removes the download directory, keeping `read`'s own error when both fail (the
/// transport's `close` then reports the directory).
fn close(
    downloaded: Downloaded,
    read: Result<StoredBundle, BundleFetchError>,
) -> Result<StoredBundle, BundleFetchError> {
    let closed = downloaded.close();
    let stored = read?;
    closed.map_err(BundleFetchError::Cleanup)?;
    Ok(stored)
}
