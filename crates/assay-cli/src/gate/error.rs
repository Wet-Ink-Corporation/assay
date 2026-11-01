//! Why `assay gate` could not run its job, or could not write its results; exit 2.

use std::fmt;
use std::io;
use std::path::PathBuf;

use assay_app::{ContractSchemaError, GateJob};
use assay_cli::OutputError;
use assay_cli::outputs::GatesOutputError;
use assay_forge::git::{GitError, InvalidHeader, ParseRemoteError};
use assay_forge::process::KillerError;
use assay_gates::{ExecSettingsError, TemperSettingsError, WeighInSettingsError};

use crate::doctor::{DoctorCliError, write_chain};
use crate::guard_job::OUTPUT;
use crate::self_gate::RUN_KEY;

/// A program `gates-static` takes by its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Program {
    /// `--node`: the `node` that runs the pinned temper.
    Node,
    /// `--temper`: the pinned temper copy fetch-by-digest prepared from the base's pin (D8 A).
    Temper,
}

impl Program {
    /// The flag that names it.
    pub(crate) const fn flag(self) -> &'static str {
        match self {
            Self::Node => "--node",
            Self::Temper => "--temper",
        }
    }
}

/// A variable `gates-static` reads besides the run key and `GITHUB_OUTPUT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variable {
    /// `ASSAY_BASE_SHA`: the pinned base commit.
    BaseSha,
    /// `ASSAY_HEAD_SHA`: the pinned head commit.
    HeadSha,
    /// `ASSAY_RUN_TOKEN`: the job's `github.token`, which the fetch authenticates with.
    RunToken,
    /// `GITHUB_REPOSITORY`: the repository the commits are fetched from.
    Repository,
    /// `PATH`, which finds git.
    Path,
}

impl Variable {
    /// The variable's name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::BaseSha => "ASSAY_BASE_SHA",
            Self::HeadSha => "ASSAY_HEAD_SHA",
            Self::RunToken => "ASSAY_RUN_TOKEN",
            Self::Repository => "GITHUB_REPOSITORY",
            Self::Path => "PATH",
        }
    }

    /// What it holds, for a refusal.
    const fn holds(self) -> &'static str {
        match self {
            Self::BaseSha => "the PR's pinned base commit, 40 lowercase hex digits",
            Self::HeadSha => "the PR's pinned head commit, 40 lowercase hex digits",
            Self::RunToken => "the job's github.token (contents: read)",
            Self::Repository => "the repository as owner/name",
            Self::Path => "the directories git is found in",
        }
    }
}

/// Which pinned commit a fetch was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// `ASSAY_BASE_SHA`.
    Base,
    /// `ASSAY_HEAD_SHA`.
    Head,
}

/// Why the job could not run, or its results could not be written; exit 2.
#[derive(Debug)]
pub(crate) enum GateCliError {
    /// Not the job's flags, each once: `--config`, `--head` and `--out`, plus `--node` and
    /// `--temper` for `gates-static` only.
    Usage,
    /// The job is named but its inputs are not wired yet.
    NotWired(GateJob),
    /// `ASSAY_RUN_KEY` is unset or empty.
    RunKey,
    /// `ASSAY_RUN_KEY` is not valid Unicode.
    RunKeyNotUnicode,
    /// What only `gates-static` takes was refused, or its commits could not be fetched.
    Static(StaticError),
    /// A path could not be resolved or inspected.
    Path { path: PathBuf, source: io::Error },
    /// `--head` is not a directory.
    HeadNotDir(PathBuf),
    /// `--config` is inside `--head`, which the PR controls.
    ConfigInHead(PathBuf),
    /// `--config` is not a regular file (a directory, a FIFO, a device).
    NotAFile(PathBuf),
    /// `--out` already exists.
    OutExists(PathBuf),
    /// `--out` is inside `--head`, where the PR's code could write first.
    OutInHead(PathBuf),
    /// `--out`'s parent is not written resolved: a symlink or `..` is on the way.
    OutNotResolved { given: PathBuf, resolved: PathBuf },
    /// The base config could not be read or was refused.
    Config(DoctorCliError),
    /// No process-group killer is available on this host.
    Killer(KillerError),
    /// The temper adapter's settings were refused.
    TemperSettings(TemperSettingsError),
    /// The exec adapter's settings were refused.
    ExecSettings(ExecSettingsError),
    /// The contract schemas did not compile.
    Validator(ContractSchemaError),
    /// The job's scratch directory could not be created or removed.
    Scratch { path: PathBuf, source: io::Error },
    /// The job failed with `error`, and its scratch directory could not be removed after
    /// (`left`, a [`GateCliError::Scratch`]).
    ScratchLeft {
        error: Box<GateCliError>,
        left: Box<GateCliError>,
    },
    /// A concluded report carries no document (the gate use case promises one).
    NoDocument(String),
    /// A document could not be encoded.
    Encode(serde_json::Error),
    /// The output directory or a document could not be written.
    Write { path: PathBuf, source: io::Error },
    /// `GITHUB_OUTPUT` is not set.
    OutputVariable,
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// The `gates` output could not be encoded; nothing was written.
    GatesOutput(GatesOutputError),
    /// The `gates` output could not be appended.
    Output(OutputError),
}

impl fmt::Display for GateCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str(
                "usage: assay gate <gates-lint|gates-test> --config <plugins-toml> --head <dir> \
                 --out <dir>, or assay gate gates-static --config <plugins-toml> --head <dir> \
                 --out <dir> --node <absolute path> --temper <absolute path>",
            ),
            Self::NotWired(job) => write!(
                f,
                "gate {job}: not wired yet (its ratchet base input is not built); gates-static \
                 and gates-test run"
            ),
            Self::RunKey => write!(f, "{RUN_KEY} is not set"),
            Self::RunKeyNotUnicode => write!(f, "{RUN_KEY} is not valid Unicode"),
            Self::Static(source) => write!(f, "{source}"),
            Self::Path { path, source } => write!(f, "cannot resolve {}: {source}", path.display()),
            Self::HeadNotDir(path) => {
                write!(f, "the head tree {} is not a directory", path.display())
            }
            Self::ConfigInHead(path) => write!(
                f,
                "the config {} is inside the head tree; base config must come from base",
                path.display()
            ),
            Self::NotAFile(path) => {
                write!(f, "the config {} is not a regular file", path.display())
            }
            Self::OutExists(path) => write!(f, "{} already exists", path.display()),
            Self::OutInHead(path) => write!(
                f,
                "the output directory {} is inside the head tree",
                path.display()
            ),
            Self::OutNotResolved { given, resolved } => write!(
                f,
                "the output directory {} passes through a symlink or `..` (its parent is {}); \
                 name it by its resolved path",
                given.display(),
                resolved.display()
            ),
            Self::Config(source) => write!(f, "{source}"),
            Self::Killer(source) => write_chain(f, source),
            Self::TemperSettings(source) => write!(f, "{source}"),
            Self::ExecSettings(source) => write!(f, "{source}"),
            Self::Validator(source) => write_chain(f, source),
            Self::Scratch { path, source } => {
                write!(f, "scratch directory {}: {source}", path.display())
            }
            Self::ScratchLeft { error, left } => write!(f, "{error}; and {left}"),
            Self::NoDocument(plugin) => write!(f, "gate `{plugin}` concluded without a document"),
            Self::Encode(source) => write!(f, "cannot encode a document: {source}"),
            Self::Write { path, source } => write!(f, "cannot write {}: {source}", path.display()),
            Self::OutputVariable => write!(f, "{OUTPUT} is not set"),
            Self::OutputFile(source) => write!(f, "opening {OUTPUT}: {source}"),
            Self::GatesOutput(source) => write!(f, "{source}"),
            Self::Output(source) => write_chain(f, source),
        }
    }
}

/// Why `gates-static` could not take its own inputs or fetch its pinned commits; exit 2.
#[derive(Debug)]
pub(crate) enum StaticError {
    /// A variable is not set.
    Missing(Variable),
    /// A variable holds something it may not; its value is never shown.
    Invalid(Variable),
    /// A program is not named by an absolute path.
    RelativeProgram { program: Program, path: PathBuf },
    /// A program resolves inside `--head`, which the PR controls.
    ProgramInHead { program: Program, path: PathBuf },
    /// The run token makes no git header.
    Authorization(InvalidHeader),
    /// The repository makes no https remote.
    Remote(ParseRemoteError),
    /// The Weigh-In adapter's settings were refused.
    WeighInSettings(WeighInSettingsError),
    /// The object-only repository could not be made.
    Git(GitError),
    /// A pinned commit could not be fetched.
    Fetch { side: Side, source: GitError },
}

impl From<StaticError> for GateCliError {
    fn from(error: StaticError) -> Self {
        Self::Static(error)
    }
}

impl fmt::Display for StaticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (context, source): (&str, &dyn std::error::Error) = match self {
            Self::Missing(variable) => {
                let (name, holds) = (variable.name(), variable.holds());
                return write!(f, "{name} is not set: it holds {holds}");
            }
            Self::Invalid(variable) => {
                let (name, holds) = (variable.name(), variable.holds());
                return write!(f, "{name} is not {holds}");
            }
            Self::RelativeProgram { program, path } => {
                let (flag, path) = (program.flag(), path.display());
                return write!(
                    f,
                    "{flag} {path} is not absolute: name it by its absolute path"
                );
            }
            Self::ProgramInHead { program, path } => {
                let (flag, path) = (program.flag(), path.display());
                return write!(
                    f,
                    "{flag} {path} is inside the head tree, which the PR controls"
                );
            }
            Self::Authorization(source) => ("", source),
            Self::Remote(source) => ("the repository's remote: ", source),
            Self::WeighInSettings(source) => ("", source),
            Self::Git(source) => ("the object-only repository: ", source),
            Self::Fetch { side, source } => match side {
                Side::Base => ("fetching the base commit: ", source),
                Side::Head => ("fetching the head commit: ", source),
            },
        };
        f.write_str(context)?;
        write_chain(f, source)
    }
}
