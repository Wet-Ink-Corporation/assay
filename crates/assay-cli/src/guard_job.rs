//! `assay guard` (#60 ST-2, 0006): the pipeline's first job. It reads the run from the Actions
//! variables, the event from `GITHUB_EVENT_NAME` and the payload `GITHUB_EVENT_PATH` names (at
//! most `MAX_EVENT_BYTES`), decides with [`assay_cli::guard`], and appends the run identity to
//! the file `GITHUB_OUTPUT` names (I2, I5).
//!
//! It holds no write token, so it does not require a first attempt. A `workflow_dispatch` names
//! only a PR, which is read through `gh` with `ASSAY_RUN_TOKEN` (the job's `github.token`,
//! `pull-requests: read`); no other event reads a token, starts a process or touches the
//! network.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::num::NonZeroU64;

use assay_app::{AppId, UserId};
use assay_cli::{Guarded, OutputError, Outputs, Refusal, RunEnv, RunEnvError, RunVariables, guard};
use assay_forge::github::{
    Api, AppIdentity, EventName, EventPayload, EventPayloadError, ForgeError, GhApi, GhEnvironment,
    GhError, GhSetupError, GhToken, GitHub, PullIdentity, Repository, ThreadSleep,
};
use assay_forge::process::{GroupKiller, KillerError};

use crate::check::on_path;
use crate::doctor::write_chain;

/// `GITHUB_EVENT_NAME`.
pub(crate) const EVENT_NAME: &str = "GITHUB_EVENT_NAME";
/// `GITHUB_EVENT_PATH`.
pub(crate) const EVENT_PATH: &str = "GITHUB_EVENT_PATH";
/// `GITHUB_OUTPUT`.
pub(crate) const OUTPUT: &str = "GITHUB_OUTPUT";
/// The job's `github.token`, read only for a dispatch.
pub(crate) const RUN_TOKEN: &str = "ASSAY_RUN_TOKEN";

/// GitHub Actions' App, whose installation token `github.token` is (app 15368; its user
/// `github-actions[bot]` is 41898282). [`GitHub::pull_identity`] attributes nothing to an App,
/// so these only name the token's owner truthfully.
const ACTIONS: AppIdentity = AppIdentity {
    app: AppId(NonZeroU64::MIN.saturating_add(15_367)),
    bot: UserId(NonZeroU64::MIN.saturating_add(41_898_281)),
};

/// The environment `assay guard` reads, gathered by `main` (ARC-11).
pub(crate) struct GuardVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`EVENT_NAME`].
    pub(crate) event_name: Option<OsString>,
    /// [`EVENT_PATH`].
    pub(crate) event_path: Option<OsString>,
    /// [`OUTPUT`].
    pub(crate) output: Option<OsString>,
    /// What a dispatch's PR read needs.
    pub(crate) dispatch: DispatchVariables,
}

/// What a dispatch reads its PR with.
pub(crate) struct DispatchVariables {
    /// [`RUN_TOKEN`].
    pub(crate) token: Option<OsString>,
    /// `PATH`, which finds `gh`.
    pub(crate) path: Option<OsString>,
    /// What `gh` inherits.
    pub(crate) gh: GhEnvironment,
}

/// How a guard that could decide ended.
pub(crate) enum Ended {
    /// The outputs are written: exit 0.
    Written,
    /// The event is refused and nothing is written: exit 1.
    Refused(Refusal),
}

/// What is wrong with a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Problem {
    /// It is not set.
    Missing,
    /// It holds something it may not; its value is never shown.
    Invalid,
}

/// Why `assay guard` could not run.
#[derive(Debug)]
pub(crate) enum GuardCliError {
    /// It was given an argument.
    Arguments,
    /// The run variables are missing or invalid.
    Run(RunEnvError),
    /// A variable is missing or invalid.
    Variable {
        name: &'static str,
        problem: Problem,
    },
    /// `GITHUB_EVENT_NAME` names an event the pipeline does not run for.
    Event,
    /// The payload file could not be opened, or is not a regular file.
    PayloadFile(io::Error),
    /// The payload was refused.
    Payload(EventPayloadError),
    /// The guard could not decide.
    Guard(assay_cli::GuardError<DispatchError>),
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// An output could not be written.
    Output(OutputError),
}

impl fmt::Display for GuardCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments => f.write_str("guard takes no arguments"),
            Self::Run(source) => write_chain(f, source),
            Self::Variable { name, problem } => match problem {
                Problem::Missing => write!(f, "{name} is not set"),
                Problem::Invalid => write!(f, "{name} is not valid"),
            },
            Self::Event => write!(
                f,
                "{EVENT_NAME}: the pipeline runs only for pull_request_target and \
                 workflow_dispatch"
            ),
            Self::PayloadFile(source) => write!(f, "reading {EVENT_PATH}: {source}"),
            Self::Payload(source) => write_chain(f, source),
            Self::Guard(source) => write_chain(f, source),
            Self::OutputFile(source) => write!(f, "opening {OUTPUT}: {source}"),
            Self::Output(source) => write_chain(f, source),
        }
    }
}

/// Why a dispatch's PR could not be read.
#[derive(Debug)]
pub(crate) enum DispatchError {
    /// A variable is missing or invalid.
    Variable {
        name: &'static str,
        problem: Problem,
    },
    /// No `gh` on `PATH`.
    NoGh,
    /// No process-group killer for `gh`'s runs.
    Killer(KillerError),
    /// gh's scratch configuration could not be prepared.
    Gh(GhSetupError),
    /// GitHub's answer was refused; `left` is gh's scratch directory, if it was not removed.
    Read {
        source: ForgeError<GhError>,
        left: Option<io::Error>,
    },
    /// The PR was read, but gh's scratch directory could not be removed.
    Left(io::Error),
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Variable { name, problem } => match problem {
                Problem::Missing => write!(f, "{name} is not set: a dispatch reads its PR with it"),
                Problem::Invalid => write!(f, "{name} is not a GitHub token"),
            },
            Self::NoGh => f.write_str("no gh on PATH: a dispatch reads its PR through gh"),
            Self::Killer(_) => f.write_str("no process-group killer for gh"),
            Self::Gh(_) => f.write_str("gh's scratch configuration could not be prepared"),
            Self::Read { left, .. } => match left {
                None => f.write_str("GitHub's answer was refused"),
                Some(left) => write!(
                    f,
                    "GitHub's answer was refused (and gh's scratch configuration directory \
                     could not be removed: {left})"
                ),
            },
            Self::Left(_) => f.write_str("could not remove gh's scratch configuration directory"),
        }
    }
}

impl Error for DispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Variable { .. } | Self::NoGh => None,
            Self::Killer(source) => Some(source),
            Self::Gh(source) => Some(source),
            Self::Read { source, .. } => Some(source),
            Self::Left(source) => Some(source),
        }
    }
}

/// Runs `assay guard` with `args` (none) and `variables`: decides, and for a run or a skip
/// appends the outputs to `GITHUB_OUTPUT`.
///
/// # Errors
///
/// [`GuardCliError`] for anything that stops it deciding or writing; nothing is written
/// before the decision.
pub(crate) fn run(
    mut args: impl Iterator<Item = OsString>,
    variables: GuardVariables,
) -> Result<Ended, GuardCliError> {
    if args.next().is_some() {
        return Err(GuardCliError::Arguments);
    }
    let RunEnv { run_uid, repo } = RunEnv::from_vars(&variables.run).map_err(GuardCliError::Run)?;
    let event = present(variables.event_name, EVENT_NAME)?
        .to_str()
        .and_then(|name| name.parse::<EventName>().ok())
        .ok_or(GuardCliError::Event)?;
    let payload = read_payload(event, present(variables.event_path, EVENT_PATH)?)?;
    let dispatch = variables.dispatch;
    let guarded = guard(run_uid.repo_id, payload, |pr| read_pull(repo, pr, dispatch))
        .map_err(GuardCliError::Guard)?;
    if let Guarded::Refused(refusal) = guarded {
        return Ok(Ended::Refused(refusal));
    }
    let file =
        open_output(present(variables.output, OUTPUT)?).map_err(GuardCliError::OutputFile)?;
    let mut outputs = Outputs::new(file);
    for (name, value) in guarded.outputs() {
        outputs.write(name, &value).map_err(GuardCliError::Output)?;
    }
    Ok(Ended::Written)
}

/// Variable `name`'s value, which must be set.
fn present(value: Option<OsString>, name: &'static str) -> Result<OsString, GuardCliError> {
    value.ok_or(GuardCliError::Variable {
        name,
        problem: Problem::Missing,
    })
}

/// The payload of `event` in the regular file at `path`.
fn read_payload(event: EventName, path: OsString) -> Result<EventPayload, GuardCliError> {
    regular_path(&path).map_err(GuardCliError::PayloadFile)?;
    let file = File::open(path).map_err(GuardCliError::PayloadFile)?;
    regular(&file).map_err(GuardCliError::PayloadFile)?;
    EventPayload::read(event, file).map_err(GuardCliError::Payload)
}

/// The regular file at `path`, `GITHUB_OUTPUT`, opened for appending (GitHub creates it; it is
/// not created). Shared with `assay gate`.
pub(crate) fn open_output(path: OsString) -> io::Result<File> {
    regular_path(&path)?;
    let file = OpenOptions::new().append(true).open(path)?;
    regular(&file)?;
    Ok(file)
}

/// Refuses a path that does not name a regular file before it is opened, so a FIFO is never
/// opened (which would block, H-13), as `check/store.rs` does (#219).
fn regular_path(path: &OsStr) -> io::Result<()> {
    if fs::metadata(path)?.is_file() {
        Ok(())
    } else {
        Err(not_regular())
    }
}

/// Refuses an open file that is not a regular one: the path may have changed since it was
/// checked.
fn regular(file: &File) -> io::Result<()> {
    if file.metadata()?.is_file() {
        Ok(())
    } else {
        Err(not_regular())
    }
}

/// The refusal of a file that is not a regular one.
fn not_regular() -> io::Error {
    io::Error::other("not a regular file")
}

/// PR `pr` of `repository`, read through `gh` with the run token; gh's scratch directory is
/// removed after the read.
fn read_pull(
    repository: Repository,
    pr: NonZeroU64,
    variables: DispatchVariables,
) -> Result<PullIdentity, DispatchError> {
    let token = token(variables.token)?;
    let program = on_path("gh", variables.path.as_ref()).ok_or(DispatchError::NoGh)?;
    let killer = GroupKiller::system().map_err(DispatchError::Killer)?;
    let gh = GhApi::new(
        assay_cli::ForgeSupervisor::new(killer),
        program,
        token,
        variables.gh,
    )
    .map_err(DispatchError::Gh)?;
    let github = GitHub::new(Api::new(gh, ThreadSleep), repository, ACTIONS);
    let read = github.pull_identity(pr);
    let left = github.into_api().into_transport().close().err();
    match (read, left) {
        (Ok(pull), None) => Ok(pull),
        (Ok(_), Some(left)) => Err(DispatchError::Left(left)),
        (Err(source), left) => Err(DispatchError::Read { source, left }),
    }
}

/// [`RUN_TOKEN`] as a token.
fn token(value: Option<OsString>) -> Result<GhToken, DispatchError> {
    let refused = |problem| DispatchError::Variable {
        name: RUN_TOKEN,
        problem,
    };
    let text = value
        .ok_or_else(|| refused(Problem::Missing))?
        .into_string()
        .map_err(|_not_utf8| refused(Problem::Invalid))?;
    GhToken::new(text).map_err(|_invalid| refused(Problem::Invalid))
}
