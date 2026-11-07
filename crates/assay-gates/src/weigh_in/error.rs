//! Why the Weigh-In adapter produced no [`GateRun`](assay_app::GateRun). The `gate` use case
//! records each as `gate-error:not-run`: a `ci-check` that did not exit 0, or a report it
//! printed that assay does not read, is never read in part (wi-b5affa).

use std::path::PathBuf;

use assay_app::{GateJob, Termination};
use thiserror::Error;

use super::report::WeighInReportError;

/// Why the Weigh-In adapter did not conclude, generic over the supervisor's error `S`.
#[derive(Debug, Error)]
pub enum WeighInError<S> {
    /// The plugin's adapter is not `builtin:weigh-in`.
    #[error("plugin `{plugin}` is not a weigh-in gate")]
    NotWeighIn {
        /// The plugin.
        plugin: String,
    },
    /// The plugin is required. The adapter runs only in experimental mode, which is
    /// non-required and never fails (04 §9), so a required one would pass on nothing.
    #[error(
        "the weigh-in plugin `{plugin}` is required, but the adapter is experimental until owner \
         policy makes it required"
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
    /// `weighin ci-check` could not be run, observed or killed.
    #[error("weighin ci-check could not be run")]
    Supervisor(#[source] S),
    /// `weighin ci-check` did not exit 0, which it does whenever it wrote its report (CLI-06).
    #[error("weighin ci-check ended without writing its report ({termination:?})")]
    Ended {
        /// How it ended.
        termination: Termination,
    },
    /// The report was refused.
    #[error(transparent)]
    Report(#[from] WeighInReportError),
    /// The document could not be serialised.
    #[error("could not write the weigh-in findings document")]
    Document(#[source] serde_json::Error),
}

/// Why [`WeighInSettings::new`](super::WeighInSettings::new) refused its settings.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WeighInSettingsError {
    /// The `weighin` program is not named by an absolute path.
    #[error("the weighin program {} is not an absolute path", path.display())]
    RelativeProgram {
        /// The refused path.
        path: PathBuf,
    },
    /// The checkout `ci-check` runs in is not an absolute path.
    #[error("the weigh-in checkout {} is not an absolute path", path.display())]
    RelativeTree {
        /// The refused path.
        path: PathBuf,
    },
    /// The ledger store `ci-check` is pointed at is not an absolute path.
    #[error("the weigh-in store {} is not an absolute path", path.display())]
    RelativeStore {
        /// The refused path.
        path: PathBuf,
    },
    /// The checkout or the store is not UTF-8, so it cannot be an argument.
    #[error("{} is not UTF-8, so weighin cannot be given it", path.display())]
    NotUtf8 {
        /// The refused path.
        path: PathBuf,
    },
    /// The gate environment's `PATH` has an empty or relative entry, which `ci-check`'s own git
    /// would resolve from the PR's tree.
    #[error("the gate environment's PATH has an empty or relative entry")]
    RelativeSearchPath,
    /// The run key is empty; the document's `source.run_key` must not be.
    #[error("the weigh-in adapter's run key is empty")]
    EmptyRunKey,
}
