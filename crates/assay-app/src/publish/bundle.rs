//! The `RunInputs` bundle `decide` persists (`assay.run-inputs/1`, 02 §5, 04 §8): everything a
//! decision replay reads, in one JSON file whose sha256 the ledger's evidence pointer records.

use std::io::{self, Write};

use assay_domain::{Decision, DecisionInputs, Event, RunUid};
use serde::Serialize;
use thiserror::Error;

use crate::decide::{AuthenticatedRun, RunDocument};
use crate::eligibility::{GitHubState, StateRecord};
use crate::plan::ArtifactJob;
use crate::rereview::CarrySet;
use crate::rereview::record::CarryOut;

/// The bundle's file in the `decide` job's artifact.
pub const BUNDLE_FILE: &str = "run-inputs.json";

/// The largest bundle `decide` writes, in bytes (96 MiB): the 64 MiB the run fetched, room for
/// JSON escaping, the decision inputs and the events (H-13).
pub const MAX_BUNDLE_BYTES: usize = 96 << 20;

/// Why the bundle could not be built. Persisting then fails (S9).
#[derive(Debug, Error)]
pub enum BundleError {
    /// A document is not UTF-8, so it cannot be kept as text without being changed.
    #[error("the {} document {file} is not UTF-8", job.name())]
    NotUtf8 {
        /// The producing job.
        job: ArtifactJob,
        /// The file in its artifact.
        file: String,
    },
    /// The bundle would be larger than [`MAX_BUNDLE_BYTES`].
    #[error("the bundle is larger than {MAX_BUNDLE_BYTES} bytes")]
    TooLarge,
    /// The bundle could not be encoded: a defect in `decide`.
    #[error("could not encode the bundle")]
    Encode(#[source] serde_json::Error),
}

/// What `decide` read for one run, as the bundle records it.
pub(super) struct Contents<'a> {
    pub(super) run_uid: RunUid,
    pub(super) run: &'a AuthenticatedRun,
    pub(super) inputs: &'a DecisionInputs,
    pub(super) decision: &'a Decision,
    pub(super) events: &'a [Event],
    pub(super) carry: &'a CarrySet,
    pub(super) github: &'a GitHubState,
}

/// `assay.run-inputs/1`.
#[derive(Serialize)]
struct RunInputs<'a> {
    schema: &'static str,
    run_uid: RunUid,
    evidence_key: &'a str,
    documents: Vec<Document<'a>>,
    inputs: &'a DecisionInputs,
    rubric_version: &'a str,
    decision: &'a Decision,
    events: Vec<&'a Event>,
    carry: CarryOut<'a>,
    github: StateRecord<'a>,
}

/// One authentic artifact document, kept as the exact text its digest covers.
#[derive(Serialize)]
struct Document<'a> {
    job: &'static str,
    file: &'a str,
    sha256: &'a str,
    text: &'a str,
}

/// The bundle's bytes: `run.json`, the gate documents and the review documents as their exact
/// text, the decision inputs, the rubric version, the decision, the events ordered by
/// `(job, idx)`, the carry set the next run reads (#224) and the GitHub state snapshot (#58).
///
/// # Errors
///
/// [`BundleError`] when a document is not UTF-8 or the bundle would be too large.
pub(super) fn bundle(contents: &Contents<'_>) -> Result<Vec<u8>, BundleError> {
    let record = contents.run.record();
    let review = record
        .review_files
        .iter()
        .zip(contents.run.review_documents())
        .map(|(file, bytes)| document(ArtifactJob::Review, &file.file, &file.digest, bytes));
    let documents = record
        .documents
        .iter()
        .map(|run_document| {
            let RunDocument {
                job,
                file,
                digest,
                bytes,
            } = run_document;
            document(*job, file, digest, bytes)
        })
        .chain(review)
        .collect::<Result<Vec<_>, _>>()?;
    let mut events: Vec<&Event> = contents.events.iter().collect();
    events.sort_by(|a, b| (a.job.as_str(), a.idx).cmp(&(b.job.as_str(), b.idx)));
    let inputs = RunInputs {
        schema: "assay.run-inputs/1",
        run_uid: contents.run_uid,
        evidence_key: &contents.inputs.evidence_key,
        documents,
        inputs: contents.inputs,
        rubric_version: &contents.decision.rubric_version,
        decision: contents.decision,
        events,
        carry: CarryOut::of(contents.carry),
        github: StateRecord::of(contents.github),
    };
    capped_json(&inputs, MAX_BUNDLE_BYTES).map_err(|error| match error {
        CappedJson::TooLarge => BundleError::TooLarge,
        CappedJson::Encode(error) => BundleError::Encode(error),
    })
}

/// Why [`capped_json`] wrote nothing.
#[derive(Debug)]
pub(crate) enum CappedJson {
    /// The JSON would be larger than the cap; it was never held whole.
    TooLarge,
    /// The value could not be encoded.
    Encode(serde_json::Error),
}

/// `value` as compact JSON of at most `cap` bytes.
pub(crate) fn capped_json<T: Serialize>(value: &T, cap: usize) -> Result<Vec<u8>, CappedJson> {
    let mut out = Capped::new(cap);
    match serde_json::to_writer(&mut out, value) {
        Ok(()) => Ok(out.bytes),
        Err(_over) if out.over => Err(CappedJson::TooLarge),
        Err(error) => Err(CappedJson::Encode(error)),
    }
}

/// A document as the bundle keeps it.
fn document<'a>(
    job: ArtifactJob,
    file: &'a str,
    digest: &'a crate::config::Sha256Digest,
    bytes: &'a [u8],
) -> Result<Document<'a>, BundleError> {
    let text = std::str::from_utf8(bytes).map_err(|_not_utf8| BundleError::NotUtf8 {
        job,
        file: file.to_owned(),
    })?;
    Ok(Document {
        job: job.name(),
        file,
        sha256: digest.as_str(),
        text,
    })
}

/// A buffer that refuses to grow past its cap, so an oversized bundle is never held whole.
struct Capped {
    bytes: Vec<u8>,
    cap: usize,
    over: bool,
}

impl Capped {
    const fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            cap,
            over: false,
        }
    }
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let fits = self
            .bytes
            .len()
            .checked_add(buf.len())
            .is_some_and(|len| len <= self.cap);
        if !fits {
            self.over = true;
            return Err(io::Error::other("the bundle is over its cap"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::{Capped, MAX_BUNDLE_BYTES};

    #[test]
    fn the_cap_holds_exactly_its_bytes() {
        let mut out = Capped::new(MAX_BUNDLE_BYTES);

        let written = out.write_all(&vec![b'x'; MAX_BUNDLE_BYTES]);

        assert_eq!(written.ok(), Some(()));
        assert_eq!(out.bytes.len(), MAX_BUNDLE_BYTES);
        assert!(!out.over, "a bundle of exactly the cap fits");
    }

    #[test]
    fn one_byte_past_the_cap_is_refused_and_not_kept() {
        let mut out = Capped::new(MAX_BUNDLE_BYTES);
        out.write_all(&vec![b'x'; MAX_BUNDLE_BYTES])
            .expect("the cap itself fits");

        let written = out.write_all(b"x");

        assert_eq!(
            written.map_err(|error| error.kind()),
            Err(std::io::ErrorKind::Other)
        );
        assert_eq!(out.bytes.len(), MAX_BUNDLE_BYTES);
        assert!(out.over, "the writer remembers it was over");
    }
}
