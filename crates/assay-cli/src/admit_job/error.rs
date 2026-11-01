//! What `assay admit` reads besides the run, and why it stops.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use assay_app::{ContractSchemaError, MAX_DOCUMENT_BYTES};
use assay_cli::{AdmitRunError, NeedsError, OutDirError, OutputError, RunEnvError};
use assay_forge::git::{GitError, InvalidHeader, ParseRemoteError, ParseShaError};
use assay_forge::github::{
    BundleFetchError, ForgeError, GhError, LedgerRefsError, LedgerRepoError,
};
use assay_forge::process::KillerError;

use super::{ARTIFACTS, CONFIG, OUT};
use crate::doctor::write_chain;
use crate::ledger_job::{LEDGER_TOKEN, LedgerSetupError, NEEDS, Problem, VariableRefusal};

/// A variable `assay admit` reads besides the run's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variable {
    /// `GITHUB_OUTPUT`: the file the outputs are appended to.
    Output,
    /// `ASSAY_NEEDS`: guard's and plan's outputs.
    Needs,
    /// `ASSAY_GITHUB_TOKEN`: the `assay-verdict` App's token.
    Token,
    /// `ASSAY_LEDGER_TOKEN`: the `assay-ledger-writer` App's token.
    LedgerToken,
    /// `ASSAY_RUN_TOKEN`: the job's `github.token`.
    RunToken,
}

impl Variable {
    /// The variable's name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Output => "GITHUB_OUTPUT",
            Self::Needs => NEEDS,
            Self::Token => "ASSAY_GITHUB_TOKEN",
            Self::LedgerToken => LEDGER_TOKEN,
            Self::RunToken => "ASSAY_RUN_TOKEN",
        }
    }

    /// What it holds, for a refusal.
    const fn holds(self) -> &'static str {
        match self {
            Self::Output => "the file the job's outputs are appended to",
            Self::Needs => "guard's and plan's outputs as ${{ toJSON(needs) }} gives them",
            Self::Token => "the assay-verdict App's installation token",
            Self::LedgerToken => "the assay-ledger-writer App's installation token",
            Self::RunToken => "the job's github.token (contents: read, actions: read)",
        }
    }
}

/// Why a pinned commit could not be fetched.
#[derive(Debug)]
pub(crate) enum FetchError {
    /// The commit is not one git accepts.
    Commit(ParseShaError),
    /// git could not fetch it.
    Git(GitError),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit(_) => f.write_str("the commit is not one git accepts"),
            Self::Git(_) => f.write_str("git could not fetch it"),
        }
    }
}

impl Error for FetchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Commit(source) => Some(source),
            Self::Git(source) => Some(source),
        }
    }
}

/// What [`assay_cli::run_admit`] fails with over the live ports.
pub(crate) type LiveFailure = AdmitRunError<
    LedgerRepoError<GhError>,
    LedgerRefsError<GhError>,
    BundleFetchError,
    ForgeError<GhError>,
    ForgeError<GhError>,
    FetchError,
>;

/// Why `assay admit` stopped; exit 2.
#[derive(Debug)]
pub(crate) enum AdmitCliError {
    /// Not exactly `--config <plugins-toml> --artifacts <dir> --out <dir>`.
    Usage,
    /// The run could not be read, or is not its first attempt.
    Run(RunEnvError),
    /// A variable is missing or invalid.
    Variable {
        variable: Variable,
        problem: Problem,
    },
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// The upstream jobs' outputs were refused.
    Needs(NeedsError),
    /// `--out` was refused.
    Out(OutDirError),
    /// plan's outputs name no plan artifact.
    NoPlanArtifact,
    /// plan's `run.json` could not be read, or is not a regular file.
    PlanArtifact { path: PathBuf, source: io::Error },
    /// plan's `run.json` is over the cap.
    PlanArtifactTooLarge { path: PathBuf },
    /// The contract schemas did not compile.
    Validator(ContractSchemaError),
    /// Base config, or the ports' setup.
    Setup(LedgerSetupError),
    /// The run token makes no git header.
    Authorization(InvalidHeader),
    /// The repository makes no https remote.
    Remote(ParseRemoteError),
    /// No `PATH` for git.
    NoGit,
    /// No process-group killer for git.
    Killer(KillerError),
    /// The scratch directory could not be made.
    Scratch(io::Error),
    /// The object-only repository could not be made.
    Git(GitError),
    /// The run could not be admitted, or its documents written.
    Admit(Box<LiveFailure>),
    /// An output could not be appended.
    Output(OutputError),
}

impl fmt::Display for AdmitCliError {
    /// `admit: `, what failed, and the cause chain.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (context, source): (&str, &dyn Error) = match self {
            Self::Usage => {
                return write!(
                    f,
                    "usage: assay admit {CONFIG} <plugins-toml> {ARTIFACTS} <dir> {OUT} <dir>"
                );
            }
            Self::Variable { variable, problem } => {
                return VariableRefusal {
                    job: "admit",
                    name: variable.name(),
                    holds: variable.holds(),
                    problem: *problem,
                }
                .fmt(f);
            }
            Self::OutputFile(source) => {
                return write!(f, "admit: opening {}: {source}", Variable::Output.name());
            }
            Self::Needs(source) => {
                write!(f, "admit: {}: ", Variable::Needs.name())?;
                return write_chain(f, source);
            }
            Self::NoPlanArtifact => return f.write_str("admit: plan names no plan artifact"),
            Self::PlanArtifactTooLarge { path } => {
                return write!(
                    f,
                    "admit: {} is over {MAX_DOCUMENT_BYTES} bytes",
                    path.display()
                );
            }
            Self::PlanArtifact { path, source } => {
                return write!(f, "admit: {}: {source}", path.display());
            }
            Self::NoGit => return f.write_str("admit: PATH is not set: it finds git"),
            Self::Setup(source) => return source.fmt(f),
            Self::Run(source) => ("", source),
            Self::Out(source) => ("", source),
            Self::Validator(source) => ("", source),
            Self::Authorization(source) => ("", source),
            Self::Remote(source) => ("the repository's remote: ", source),
            Self::Killer(source) => ("", source),
            Self::Scratch(source) => ("the scratch directory: ", source),
            Self::Git(source) => ("the object-only repository: ", source),
            Self::Admit(source) => ("", source.as_ref()),
            Self::Output(source) => ("", source),
        };
        write!(f, "admit: {context}")?;
        write_chain(f, source)
    }
}
