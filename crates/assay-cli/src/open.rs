//! `assay open` (#54, #60 ST-4, S0): the run from the Actions environment through [`RunEnv`],
//! refused for a re-run before any token is read (R3-05, I1), and [`run_open`], which opens
//! the verdict check for the identity `guard` resolved over any ports, so it is tested with
//! fakes.

mod run;

use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;

pub use run::{OpenJob, OpenReads, OpenRunError, OpenRunOutcome, run_open};

use crate::ci::{FirstAttempt, RunEnv, RunEnvError, RunVariables};

/// Why `assay open` would not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenUsage {
    /// Its arguments are not exactly `--config <plugins-toml>`.
    Arguments,
    /// A variable is not set.
    Missing(&'static str),
    /// A variable does not parse; its value is not shown.
    Invalid(&'static str),
    /// The run is a re-run: every job holding a write token runs only on attempt 1, and
    /// recovery is a fresh run (02 §1, R3-05).
    NotFirstAttempt {
        /// The attempt.
        attempt: NonZeroU64,
    },
}

impl From<RunEnvError> for OpenUsage {
    fn from(error: RunEnvError) -> Self {
        match error {
            RunEnvError::Missing(name) => Self::Missing(name),
            RunEnvError::Invalid(name) => Self::Invalid(name),
            RunEnvError::NotFirstAttempt(refusal) => Self::NotFirstAttempt {
                attempt: refusal.attempt,
            },
        }
    }
}

impl fmt::Display for OpenUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments => f.write_str("usage: assay open --config <plugins-toml>"),
            Self::Missing(name) => write!(f, "open: {name} is not set"),
            Self::Invalid(name) => write!(f, "open: {name} is not valid"),
            Self::NotFirstAttempt { attempt } => write!(
                f,
                "open refuses run attempt {attempt}: only a first attempt runs; start a new run"
            ),
        }
    }
}

impl std::error::Error for OpenUsage {}

/// What `assay open` runs for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCommand {
    /// The workflow run, shown to be its first attempt.
    pub run: FirstAttempt,
    /// Base `.assay/plugins.toml`, which names the ledger repository.
    pub config: PathBuf,
}

impl OpenCommand {
    /// Reads the arguments after `open`, exactly `--config <plugins-toml>`, then the run from
    /// `variables` as [`RunEnv::from_vars`] reads it, which must be a first attempt.
    ///
    /// # Errors
    ///
    /// [`OpenUsage::Arguments`] for any other arguments; [`OpenUsage::Missing`] and
    /// [`OpenUsage::Invalid`] name a variable that is unset or does not parse; and
    /// [`OpenUsage::NotFirstAttempt`] for a run attempt other than 1.
    pub fn parse(
        args: impl IntoIterator<Item = OsString>,
        variables: &RunVariables,
    ) -> Result<Self, OpenUsage> {
        let mut args = args.into_iter();
        let config = match (args.next(), args.next(), args.next()) {
            (Some(flag), Some(path), None) if flag == "--config" => PathBuf::from(path),
            _ => return Err(OpenUsage::Arguments),
        };
        let run = RunEnv::from_vars(variables)?
            .first_attempt()
            .map_err(RunEnvError::from)?;
        Ok(Self { run, config })
    }
}
