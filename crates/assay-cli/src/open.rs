//! `assay open` (#54, S0): the run's identity from the Actions environment, refused for a
//! re-run before any token is minted (R3-05, 02 §1). The use case is [`assay_app::open`]; the
//! binary wires it once the live transport (#273) exists.

use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;

use assay_domain::RunUid;

/// The Actions variables `assay open` reads, as the environment holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenVariables {
    /// `GITHUB_RUN_ID`.
    pub run_id: Option<OsString>,
    /// `GITHUB_RUN_ATTEMPT`.
    pub run_attempt: Option<OsString>,
    /// `GITHUB_REPOSITORY_ID`.
    pub repository_id: Option<OsString>,
}

/// Why `assay open` would not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenUsage {
    /// It was given arguments; it takes none.
    Arguments,
    /// A variable is not set.
    Missing(&'static str),
    /// A variable is not a positive decimal number.
    Invalid(&'static str),
    /// The run is a re-run: every job holding a write token runs only on attempt 1, and
    /// recovery is a fresh run (02 §1, R3-05).
    NotFirstAttempt {
        /// The attempt.
        attempt: NonZeroU64,
    },
}

impl fmt::Display for OpenUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments => f.write_str("open takes no arguments; usage: assay open"),
            Self::Missing(name) => write!(f, "open: {name} is not set"),
            Self::Invalid(name) => write!(f, "open: {name} is not a positive number"),
            Self::NotFirstAttempt { attempt } => write!(
                f,
                "open refuses run attempt {attempt}: only a first attempt runs; start a new run"
            ),
        }
    }
}

impl std::error::Error for OpenUsage {}

/// What `assay open` runs for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenCommand {
    /// The workflow run, always its first attempt.
    pub run_uid: RunUid,
}

impl OpenCommand {
    /// Reads the run from `variables`; `args` are those after `open`.
    ///
    /// # Errors
    ///
    /// [`OpenUsage`] for any argument, a missing or malformed variable, and a run attempt other
    /// than 1.
    pub fn parse(
        args: impl IntoIterator<Item = OsString>,
        variables: OpenVariables,
    ) -> Result<Self, OpenUsage> {
        if args.into_iter().next().is_some() {
            return Err(OpenUsage::Arguments);
        }
        let run_uid = RunUid {
            repo_id: positive(variables.repository_id, "GITHUB_REPOSITORY_ID")?,
            run_id: positive(variables.run_id, "GITHUB_RUN_ID")?,
            run_attempt: positive(variables.run_attempt, "GITHUB_RUN_ATTEMPT")?,
        };
        if run_uid.run_attempt.get() != 1 {
            return Err(OpenUsage::NotFirstAttempt {
                attempt: run_uid.run_attempt,
            });
        }
        Ok(Self { run_uid })
    }
}

/// Variable `name`'s value as a positive number: ASCII digits only, not zero.
fn positive(value: Option<OsString>, name: &'static str) -> Result<NonZeroU64, OpenUsage> {
    let value = value.ok_or(OpenUsage::Missing(name))?;
    value
        .to_str()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse().ok())
        .ok_or(OpenUsage::Invalid(name))
}
