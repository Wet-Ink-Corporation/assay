//! Why the Weigh-In adapter produced no [`GateRun`](assay_app::GateRun). The `gate` use case
//! records each as `gate-error:not-run`. A journal or atom the adapter cannot read is not an
//! error: it is a `weigh-in-unreadable` finding.

use assay_app::GateJob;
use thiserror::Error;

/// Why the Weigh-In adapter did not conclude, generic over the file reader's error `R`.
#[derive(Debug, Error)]
pub enum WeighInError<R> {
    /// The plugin's adapter is not `builtin:weigh-in`.
    #[error("plugin `{plugin}` is not a weigh-in gate")]
    NotWeighIn {
        /// The plugin.
        plugin: String,
    },
    /// The plugin is required. Until D-WI the adapter runs only in experimental mode, which is
    /// non-required and never fails (04 §9), so a required one would pass on nothing.
    #[error(
        "the weigh-in plugin `{plugin}` is required, but the adapter is experimental until D-WI"
    )]
    Required {
        /// The plugin.
        plugin: String,
    },
    /// The job is not `gates-static`, the only job weigh-in is scheduled in.
    #[error("the weigh-in adapter runs in gates-static, not {job}")]
    OutsideStatic {
        /// The job asked for.
        job: GateJob,
    },
    /// The head tree could not be listed or read at `path`.
    #[error("could not read `{path}` at head")]
    Revision {
        /// The directory listed, or `.kb` for the batch read.
        path: &'static str,
        /// The reader's error.
        #[source]
        source: R,
    },
    /// The reader returned another number of blobs than it was asked for.
    #[error("the reader returned {read} blobs for {asked} files")]
    BlobCount {
        /// Files asked for.
        asked: usize,
        /// Blobs returned.
        read: usize,
    },
    /// The document could not be serialised.
    #[error("could not write the weigh-in findings document")]
    Document(#[source] serde_json::Error),
}

/// Why [`WeighInAdapter::new`](super::WeighInAdapter::new) refused its settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WeighInSettingsError {
    /// The run key is empty; the document's `source.run_key` must not be.
    #[error("the weigh-in adapter's run key is empty")]
    EmptyRunKey,
}
