//! Reading the run's artifacts under a byte budget and authenticating each by its producing
//! job's output digest (02 §1, F18).

use super::DecideError;
use super::ports::{Fetched, RunArtifacts};
use crate::config::Sha256Digest;
use crate::contract::MAX_DOCUMENT_BYTES;
use crate::plan::ArtifactJob;

/// The most bytes one `decide` run fetches across every artifact (H-13).
pub const MAX_FETCHED_BYTES: usize = 64 << 20;

/// What one fetch established.
pub(super) enum Fetch {
    /// The bytes, whose digest matches the producing job's output.
    Authentic(Vec<u8>),
    /// The file is gone.
    Missing,
    /// The file is larger than one document or the run's remaining budget.
    TooLarge,
    /// The bytes do not hash to the producing job's output: never used.
    Tampered,
}

/// One job's artifact, by name.
#[derive(Debug, Clone, Copy)]
pub(super) struct Artifact<'a> {
    /// The producing job.
    pub(super) job: ArtifactJob,
    /// The artifact's name, as the authenticated plan gives it.
    pub(super) name: &'a str,
}

/// The run's artifact store with what is left of the byte budget.
pub(super) struct Fetcher<'a, A> {
    artifacts: &'a A,
    remaining: usize,
}

impl<'a, A: RunArtifacts> Fetcher<'a, A> {
    pub(super) const fn new(artifacts: &'a A) -> Self {
        Self {
            artifacts,
            remaining: MAX_FETCHED_BYTES,
        }
    }

    /// Fetches `file` of `artifact` and checks it hashes to `digest`.
    pub(super) fn fetch<O, S>(
        &mut self,
        artifact: Artifact<'_>,
        file: &str,
        digest: &Sha256Digest,
    ) -> Result<Fetch, DecideError<O, S, A::Error>> {
        let limit = MAX_DOCUMENT_BYTES.min(self.remaining);
        let fetched = self
            .artifacts
            .fetch(artifact.name, file, limit)
            .map_err(|source| DecideError::Artifacts {
                job: artifact.job,
                file: file.to_owned(),
                source,
            })?;
        Ok(match fetched {
            Fetched::Missing => Fetch::Missing,
            Fetched::TooLarge => Fetch::TooLarge,
            // The adapter is held to the limit too: more than asked for is not read on.
            Fetched::Found(bytes) if bytes.len() > limit => Fetch::TooLarge,
            Fetched::Found(bytes) => {
                self.remaining = self.remaining.saturating_sub(bytes.len());
                if Sha256Digest::of(&bytes) == *digest {
                    Fetch::Authentic(bytes)
                } else {
                    Fetch::Tampered
                }
            }
        })
    }
}
