//! What `assay plan` reads besides the run, why it stops, and what it can leave behind.

use std::error::Error;
use std::fmt;
use std::io;

use assay_app::{ContractSchemaError, InvalidBundle};
use assay_cli::{NeedsError, OutDirError, OutputError, PlanRunError, RunEnvError};
use assay_forge::git::{GitError, InvalidHeader, ParseRemoteError, ParseShaError};
use assay_forge::github::{ForgeError, GhError, GhSetupError};
use assay_forge::process::KillerError;

use super::OUT;
use crate::doctor::write_chain;

/// A variable `assay plan` reads besides the run's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variable {
    /// `GITHUB_OUTPUT`: the file the outputs are appended to.
    Output,
    /// `ASSAY_NEEDS`: guard's outputs.
    Needs,
    /// `ASSAY_RUN_TOKEN`: the job's `github.token`.
    RunToken,
}

impl Variable {
    /// The variable's name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Output => "GITHUB_OUTPUT",
            Self::Needs => "ASSAY_NEEDS",
            Self::RunToken => "ASSAY_RUN_TOKEN",
        }
    }

    /// What it holds, for a refusal.
    const fn holds(self) -> &'static str {
        match self {
            Self::Output => "the file the job's outputs are appended to",
            Self::Needs => "guard's outputs as ${{ toJSON(needs) }} gives them",
            Self::RunToken => "the job's github.token (contents: read, pull-requests: read)",
        }
    }
}

/// Which pinned commit a fetch was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// `base_sha`.
    Base,
    /// `head_sha`.
    Head,
}

/// Why `assay plan` wrote no outputs; exit 2.
#[derive(Debug)]
pub(crate) enum PlanCliError {
    /// Not exactly `--out <dir>`.
    Usage,
    /// The run variables are missing or invalid.
    Run(RunEnvError),
    /// A variable is not set.
    Missing(Variable),
    /// A variable holds something it may not; its value is never shown.
    Invalid(Variable),
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// guard's outputs were refused.
    Needs(NeedsError),
    /// `--out` was refused.
    Out(OutDirError),
    /// The embedded prompt bundle was refused.
    Bundle(InvalidBundle),
    /// The contract schemas did not compile.
    Validator(ContractSchemaError),
    /// No `gh` on `PATH`.
    NoGh,
    /// No process-group killer for `gh` and git.
    Killer(KillerError),
    /// gh's scratch configuration could not be prepared.
    Gh(GhSetupError),
    /// GitHub's file list was refused or could not be read.
    Files(ForgeError<GhError>),
    /// The run token makes no git header.
    Authorization(InvalidHeader),
    /// The repository makes no https remote.
    Remote(ParseRemoteError),
    /// A pinned commit is not one git accepts.
    Commit(ParseShaError),
    /// The scratch directory could not be made.
    Scratch(io::Error),
    /// The object-only repository could not be made.
    Git(GitError),
    /// A pinned commit could not be fetched.
    Fetch { side: Side, source: GitError },
    /// The run could not be planned or its document written.
    Plan(Box<PlanRunError<io::Error>>),
    /// An output could not be appended.
    Output(OutputError),
}

impl fmt::Display for PlanCliError {
    /// `plan: `, what failed, and the cause chain.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (context, source): (&str, &dyn Error) = match self {
            Self::Usage => return write!(f, "usage: assay plan {OUT} <dir>"),
            Self::Missing(variable) => {
                let (name, holds) = (variable.name(), variable.holds());
                return write!(f, "plan: {name} is not set: it holds {holds}");
            }
            Self::Invalid(variable) => {
                let (name, holds) = (variable.name(), variable.holds());
                return write!(f, "plan: {name} is not {holds}");
            }
            Self::NoGh => {
                return f.write_str("plan: no gh on PATH: plan reads the PR's files through gh");
            }
            Self::OutputFile(source) => {
                return write!(f, "plan: opening {}: {source}", Variable::Output.name());
            }
            Self::Needs(source) => {
                write!(f, "plan: {}: ", Variable::Needs.name())?;
                return write_chain(f, source);
            }
            Self::Run(source) => ("", source),
            Self::Out(source) => ("", source),
            Self::Bundle(source) => ("the embedded prompt bundle: ", source),
            Self::Validator(source) => ("", source),
            Self::Killer(source) => ("", source),
            Self::Gh(source) => ("", source),
            Self::Files(source) => ("the PR's file list: ", source),
            Self::Authorization(source) => ("", source),
            Self::Remote(source) => ("the repository's remote: ", source),
            Self::Commit(source) => ("a pinned commit: ", source),
            Self::Scratch(source) => ("the scratch directory: ", source),
            Self::Git(source) => ("the object-only repository: ", source),
            Self::Fetch { side, source } => match side {
                Side::Base => ("fetching the base commit: ", source),
                Side::Head => ("fetching the head commit: ", source),
            },
            Self::Plan(source) => ("", source.as_ref()),
            Self::Output(source) => ("", source),
        };
        write!(f, "plan: {context}")?;
        write_chain(f, source)
    }
}

/// A scratch directory that could not be removed.
#[derive(Debug)]
pub(crate) enum Left {
    /// gh's scratch configuration directory.
    Gh(io::Error),
    /// The object-only repository's scratch directory.
    Scratch(io::Error),
}

impl fmt::Display for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gh(source) => {
                write!(
                    f,
                    "plan: could not remove gh's scratch configuration directory: {source}"
                )
            }
            Self::Scratch(source) => {
                write!(
                    f,
                    "plan: could not remove plan's scratch directory: {source}"
                )
            }
        }
    }
}
