//! What `assay decide` and `assay publish` read besides the run, and why they stop.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;

use assay_app::{
    ContractSchemaError, InvalidBundle, LedgerPublishError, ParseLedgerTimeError, RubricError,
};
use assay_cli::{
    DecideRunError, DirArtifactsError, NeedsError, OutDirError, OutputError, PublishRunError,
    RunEnvError,
};
use assay_forge::git::{GitError, InvalidHeader, ParseRemoteError};
use assay_forge::github::{
    BundleFetchError, ForgeError, GhError, LedgerRefsError, LedgerRepoError,
};
use assay_forge::process::KillerError;

use crate::admit_job::FetchError;
use crate::doctor::write_chain;
use crate::ledger_job::{LEDGER_TOKEN, LedgerSetupError, NEEDS, Problem, VariableRefusal};
use crate::status_pr::{APP_ID, BOT_ID, TOKEN};

/// A variable `assay decide` or `assay publish` reads besides the run's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variable {
    /// `GITHUB_OUTPUT`: the file decide's outputs are appended to.
    Output,
    /// `ASSAY_NEEDS`: every upstream job's outputs.
    Needs,
    /// `ASSAY_GITHUB_TOKEN`: the `assay-verdict` App's token.
    Token,
    /// `ASSAY_LEDGER_TOKEN`: the `assay-ledger-writer` App's token.
    LedgerToken,
    /// `ASSAY_RUN_TOKEN`: the job's `github.token`.
    RunToken,
    /// `ASSAY_VERDICT_APP_ID`: the `assay-verdict` App's id.
    AppId,
    /// `ASSAY_VERDICT_BOT_ID`: the id of the `assay-verdict` App's bot user.
    BotId,
    /// `ASSAY_PERSIST`: how the upload step ended, publish only.
    Persist,
}

impl Variable {
    /// The variable's name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Output => "GITHUB_OUTPUT",
            Self::Needs => NEEDS,
            Self::Token => TOKEN,
            Self::LedgerToken => LEDGER_TOKEN,
            Self::RunToken => "ASSAY_RUN_TOKEN",
            Self::AppId => APP_ID,
            Self::BotId => BOT_ID,
            Self::Persist => "ASSAY_PERSIST",
        }
    }

    /// What it holds, for a refusal.
    const fn holds(self) -> &'static str {
        match self {
            Self::Output => "the file the job's outputs are appended to",
            Self::Needs => "every upstream job's outputs as ${{ toJSON(needs) }} gives them",
            Self::Token => "the assay-verdict App's installation token",
            Self::LedgerToken => "the assay-ledger-writer App's installation token",
            Self::RunToken => "the job's github.token (contents: read, actions: read)",
            Self::AppId => "the assay-verdict App's id, a positive decimal number",
            Self::BotId => "the id of the assay-verdict App's bot user, a positive decimal number",
            Self::Persist => "the upload step's outcome: success, failure, cancelled or skipped",
        }
    }

    /// Its refusal in `job`.
    pub(crate) const fn refusal(
        self,
        job: &'static str,
        problem: Problem,
    ) -> VariableRefusal<'static> {
        VariableRefusal {
            job,
            name: self.name(),
            holds: self.holds(),
            problem,
        }
    }
}

/// What [`assay_cli::run_decide`] fails with over the live ports.
pub(crate) type DecideFailure = DecideRunError<
    FetchError,
    ForgeError<GhError>,
    DirArtifactsError,
    ForgeError<GhError>,
    LedgerRepoError<GhError>,
    BundleFetchError,
    ForgeError<GhError>,
>;

/// What [`assay_cli::run_publish`] fails with over the live ports.
pub(crate) type PublishFailure = PublishRunError<
    ForgeError<GhError>,
    LedgerPublishError<LedgerRepoError<GhError>, LedgerRefsError<GhError>, ParseLedgerTimeError>,
>;

/// Why `assay decide` stopped; exit 2.
#[derive(Debug)]
pub(crate) enum DecideCliError {
    /// Not exactly `--config <plugins-toml> --artifacts <dir> --out <dir>`.
    Usage,
    /// What both commands refuse alike.
    Common(Common),
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// `--out` was refused.
    Out(OutDirError),
    /// The contract schemas did not compile.
    Validator(ContractSchemaError),
    /// The embedded prompt bundle is not one.
    Bundle(InvalidBundle),
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
    /// The PR's file list could not be read.
    Files(ForgeError<GhError>),
    /// The run could not be decided, or its state written.
    Decide(Box<DecideFailure>),
    /// An output could not be appended.
    Output(OutputError),
}

/// Why `assay publish` stopped; exit 2.
#[derive(Debug)]
pub(crate) enum PublishCliError {
    /// Not exactly `--config <plugins-toml> --state <dir>`.
    Usage,
    /// What both commands refuse alike.
    Common(Common),
    /// The verdict could not be published.
    Publish(Box<PublishFailure>),
}

/// What `decide` and `publish` refuse alike.
#[derive(Debug)]
pub(crate) enum Common {
    /// The run could not be read, or is not its first attempt.
    Run(RunEnvError),
    /// A variable is missing or invalid.
    Variable {
        variable: Variable,
        problem: Problem,
    },
    /// The upstream jobs' outputs were refused.
    Needs(NeedsError),
    /// The embedded rubric did not parse.
    Rubric(RubricError),
    /// Base config, or the ports' setup.
    Setup(LedgerSetupError),
}

impl Common {
    /// `job: `, what failed, and the cause chain.
    pub(crate) fn write(&self, job: &'static str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Variable { variable, problem } => variable.refusal(job, *problem).fmt(f),
            Self::Needs(source) => {
                write!(f, "{job}: {}: ", Variable::Needs.name())?;
                write_chain(f, source)
            }
            Self::Setup(source) => source.fmt(f),
            Self::Run(source) => {
                write!(f, "{job}: ")?;
                write_chain(f, source)
            }
            Self::Rubric(source) => {
                write!(f, "{job}: the embedded rubric: ")?;
                write_chain(f, source)
            }
        }
    }
}

impl Display for DecideCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (context, source): (&str, &dyn Error) = match self {
            Self::Usage => {
                return f.write_str(
                    "usage: assay decide --config <plugins-toml> --artifacts <dir> --out <dir>",
                );
            }
            Self::Common(common) => return common.write("decide", f),
            Self::OutputFile(source) => {
                return write!(f, "decide: opening {}: {source}", Variable::Output.name());
            }
            Self::NoGit => return f.write_str("decide: PATH is not set: it finds git"),
            Self::Out(source) => ("", source),
            Self::Validator(source) => ("", source),
            Self::Bundle(source) => ("the embedded prompt bundle: ", source),
            Self::Authorization(source) => ("", source),
            Self::Remote(source) => ("the repository's remote: ", source),
            Self::Killer(source) => ("", source),
            Self::Scratch(source) => ("the scratch directory: ", source),
            Self::Git(source) => ("the object-only repository: ", source),
            Self::Files(source) => ("the PR's file list: ", source),
            Self::Decide(source) => ("", source.as_ref()),
            Self::Output(source) => ("", source),
        };
        write!(f, "decide: {context}")?;
        write_chain(f, source)
    }
}

impl Display for PublishCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => {
                f.write_str("usage: assay publish --config <plugins-toml> --state <dir>")
            }
            Self::Common(common) => common.write("publish", f),
            Self::Publish(source) => {
                f.write_str("publish: ")?;
                write_chain(f, source.as_ref())
            }
        }
    }
}
