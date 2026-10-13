//! Why the temper adapter produced no [`GateRun`](assay_app::GateRun). Every one fails closed:
//! the `gate` use case records it as `gate-error:not-run`, or a degraded ratchet as
//! `gate-error:ratchet-degraded`, never as a pass.

use std::io;
use std::path::PathBuf;

use assay_app::{GateJob, Termination};
use thiserror::Error;

use super::ratchet::RatchetDegradation;
use crate::revision::Revision;

/// Why the temper adapter did not conclude, generic over the supervisor's error `S` and the
/// file reader's error `R`.
#[derive(Debug, Error)]
pub enum TemperError<S, R> {
    /// The plugin's adapter is not `builtin:temper`.
    #[error("plugin `{plugin}` is not a temper gate")]
    NotTemper {
        /// The plugin.
        plugin: String,
    },
    /// The job is `gates-test`, whose declared steps are not built yet (#67).
    #[error("the temper adapter does not run in {job} yet")]
    UnsupportedJob {
        /// The job asked for.
        job: GateJob,
    },
    /// `gates-lint` was asked for without the base SHA and the merge base Z2s computed
    /// ([`TemperSettings::with_ratchet`](super::TemperSettings::with_ratchet)).
    #[error("the temper adapter has no ratchet base for gates-lint")]
    NoRatchetBase,
    /// The ratchet did not diff against the expected merge base: recorded as
    /// `gate-error:ratchet-degraded`, never believed.
    #[error("the temper ratchet is degraded: {why}")]
    RatchetDegraded {
        /// What gave it away.
        why: RatchetDegradation,
    },
    /// The findings document could not be cleared before the run or read after it.
    #[error("could not prepare or read the temper findings document {}", path.display())]
    FindingsOut {
        /// The document's path.
        path: PathBuf,
        /// The file system's error.
        #[source]
        source: io::Error,
    },
    /// The findings document is absent, not a regular file, too large, not the shape temper
    /// 1.3.0 writes, or disagrees with temper's exit code; the detail never quotes it.
    #[error("the temper findings document is unusable: {detail}")]
    FindingsOutput {
        /// What was wrong.
        detail: String,
    },
    /// The supervisor could not run `baseline --check` or a declared step.
    #[error("could not run {process} in gates-lint")]
    Lint {
        /// `baseline --check`, or the step's name.
        process: String,
        /// The supervisor's error.
        #[source]
        source: S,
    },
    /// A file or the tree of the PR could not be read.
    #[error("could not read {path} at {revision}")]
    Revision {
        /// The side read.
        revision: Revision,
        /// The path read; `.` for the whole tree.
        path: &'static str,
        /// The reader's error.
        #[source]
        source: R,
    },
    /// A file the check compares is absent on one side.
    #[error("{path} is missing at {revision}")]
    Missing {
        /// The side it is missing from.
        revision: Revision,
        /// The path.
        path: &'static str,
    },
    /// The path holds a directory, a symlink or a submodule rather than one regular file.
    #[error("{path} at {revision} is not a regular file")]
    NotAFile {
        /// The side.
        revision: Revision,
        /// The path.
        path: &'static str,
    },
    /// A file does not parse. For a PR-controlled file the detail never quotes it.
    #[error("{path} at {revision} is unreadable: {detail}")]
    Malformed {
        /// The side.
        revision: Revision,
        /// The path.
        path: &'static str,
        /// What was wrong.
        detail: String,
    },
    /// The supervisor could not run the pinned scan.
    #[error("could not run the pinned temper scan")]
    Supervisor(#[source] S),
    /// The scan did not exit 0 or 1 (exit 1 means it found `hard` hits).
    #[error("the pinned temper scan ended with {termination:?}")]
    ScanEnded {
        /// How it ended.
        termination: Termination,
    },
    /// The scan's output is not the array of hits temper prints; the detail never quotes it.
    #[error("the pinned temper scan printed unreadable output: {detail}")]
    ScanOutput {
        /// What was wrong.
        detail: String,
    },
    /// The pinned temper copy could not be read whole.
    #[error("could not read the pinned temper at {}", path.display())]
    Pinned {
        /// The file or directory that failed.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The blob ids of the pinned copy, which its drift is compared by, could not be computed.
    #[error("could not compute the blob ids of the pinned temper")]
    BlobId(#[source] R),
    /// The findings document could not be serialised.
    #[error("could not write the findings document")]
    Document(#[source] serde_json::Error),
}

/// Why [`TemperSettings`](super::TemperSettings) were refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemperSettingsError {
    /// The `node` binary is a relative path: it would be looked up on a `PATH` or resolved
    /// from the head tree, which the PR controls.
    #[error("node {} is not an absolute path", path.display())]
    RelativeNode {
        /// The refused path.
        path: PathBuf,
    },
    /// The gate environment's `PATH` has an empty or relative entry, which the scan's own
    /// child processes (git) would resolve from the head tree.
    #[error("the gate environment's PATH has an empty or relative entry")]
    RelativeSearchPath,
    /// The pinned temper directory is a relative path.
    #[error("the pinned temper {} is not an absolute path", path.display())]
    RelativePinned {
        /// The refused path.
        path: PathBuf,
    },
    /// The pinned temper directory cannot be passed to `node` as text.
    #[error("the pinned temper {} is not UTF-8", path.display())]
    NonUtf8Pinned {
        /// The refused path.
        path: PathBuf,
    },
    /// The head tree is relative, or is the root directory (git's discovery cannot be capped
    /// above it).
    #[error("the head tree {} is not an absolute path below the root", path.display())]
    HeadTree {
        /// The refused path.
        path: PathBuf,
    },
    /// The run key is empty; the findings document requires one.
    #[error("the run key is empty")]
    EmptyRunKey,
}
