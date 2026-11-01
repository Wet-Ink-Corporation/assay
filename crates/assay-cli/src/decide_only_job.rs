//! `assay decide-only <step>` (#61, D9 A): the decide-only workflow's steps, like
//! `assay gate <job>`. `guard` (DO-10) is here; `decide` (DO-11) is `crate::redecide_job` and
//! `publish` (DO-12) `crate::republish_job`, which `main` dispatches to before this module.
//!
//! `guard` is the workflow's first job and holds no secret (0006, J3). It reads the run from the
//! Actions variables, the `assay-verdict` App's ids (`ASSAY_VERDICT_APP_ID`,
//! `ASSAY_VERDICT_BOT_ID`: variables, not secrets), the event from `GITHUB_EVENT_NAME` and the
//! payload `GITHUB_EVENT_PATH` names, decides with [`assay_cli::guard_decide_only`], and
//! appends its outputs to the file `GITHUB_OUTPUT` names (03 §4a). GitHub is read only when
//! the decision needs it (a command comment's PR, the poll), through `gh` with
//! `ASSAY_RUN_TOKEN` (the job's `github.token`) and no other token; an event that is no reason
//! to decide reads no token and starts no process.

use std::cell::{Cell, OnceCell};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::num::NonZeroU64;

use assay_app::{AppId, CheckName, CommitId, PollForge, PolledPull, Review, UserId, VerdictView};
use assay_cli::{
    DecideOnlyGuardJob, DecideOnlyGuardReads, ForgeSupervisor, GuardDecideOnlyError,
    GuardedDecideOnly, OutputError, Outputs, Refusal, RunEnv, RunEnvError, RunVariables,
    guard_decide_only,
};
use assay_forge::github::{
    Api, AppIdentity, DecideOnlyEvent, DecideOnlyEventError, DecideOnlyEventName, ForgeError,
    GhApi, GhEnvironment, GhError, GhSetupError, GitHub, PullIdentity, Repository, ThreadSleep,
};
use assay_forge::process::{GroupKiller, KillerError};

use crate::check::on_path;
use crate::doctor::write_chain;
use crate::guard_job::{EVENT_NAME, EVENT_PATH, OUTPUT, RUN_TOKEN, open_output, open_payload};
use crate::ledger_job::{self, Problem};
use crate::status_pr::{APP_ID, BOT_ID};

/// The usage line.
const USAGE: &str = "usage: assay decide-only guard";

/// The environment `assay decide-only guard` reads, gathered by `main` (ARC-11).
pub(crate) struct DecideOnlyVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`EVENT_NAME`].
    pub(crate) event_name: Option<OsString>,
    /// [`EVENT_PATH`].
    pub(crate) event_path: Option<OsString>,
    /// [`OUTPUT`].
    pub(crate) output: Option<OsString>,
    /// [`APP_ID`].
    pub(crate) app_id: Option<OsString>,
    /// [`BOT_ID`].
    pub(crate) bot_id: Option<OsString>,
    /// What a read of GitHub needs.
    pub(crate) reads: ReadVariables,
}

/// What GitHub is read with, when the decision needs it.
pub(crate) struct ReadVariables {
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

/// Why `assay decide-only` could not run.
#[derive(Debug)]
pub(crate) enum DecideOnlyCliError {
    /// The arguments are not `guard`.
    Usage,
    /// The run variables are missing or invalid.
    Run(RunEnvError),
    /// A variable is missing or invalid; its value is never shown.
    Variable {
        name: &'static str,
        problem: Problem,
    },
    /// `GITHUB_EVENT_NAME` names an event decide-only does not run for.
    Event,
    /// The payload file could not be opened, or is not a regular file.
    PayloadFile(io::Error),
    /// The payload was refused.
    Payload(DecideOnlyEventError),
    /// The guard could not decide; `left` is gh's scratch directory, if it was not removed.
    Guard {
        source: GuardDecideOnlyError<ReadError>,
        left: Option<io::Error>,
    },
    /// The guard decided, but gh's scratch directory could not be removed.
    Left(io::Error),
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// An output could not be written.
    Output(OutputError),
}

impl fmt::Display for DecideOnlyCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str(USAGE),
            Self::Run(source) => write_chain(f, source),
            Self::Variable { name, problem } => match problem {
                Problem::Missing => write!(f, "{name} is not set"),
                Problem::Invalid => write!(f, "{name} is not a positive decimal id"),
            },
            Self::Event => write!(
                f,
                "{EVENT_NAME}: decide-only runs only for issue_comment, pull_request_target and \
                 schedule"
            ),
            Self::PayloadFile(source) => write!(f, "reading {EVENT_PATH}: {source}"),
            Self::Payload(source) => write_chain(f, source),
            Self::Guard { source, left } => {
                write_chain(f, source)?;
                match left {
                    None => Ok(()),
                    Some(left) => write!(
                        f,
                        " (and gh's scratch configuration directory could not be removed: \
                         {left})"
                    ),
                }
            }
            Self::Left(source) => write!(
                f,
                "could not remove gh's scratch configuration directory: {source}"
            ),
            Self::OutputFile(source) => write!(f, "opening {OUTPUT}: {source}"),
            Self::Output(source) => write_chain(f, source),
        }
    }
}

/// Why GitHub could not be read.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// [`RUN_TOKEN`] is missing or not a token.
    Token(Problem),
    /// No `gh` on `PATH`.
    NoGh,
    /// No process-group killer for `gh`'s runs.
    Killer(KillerError),
    /// gh's scratch configuration could not be prepared.
    Gh(GhSetupError),
    /// gh could not be set up by an earlier read, so this one was not made.
    Unavailable,
    /// GitHub's answer was refused.
    Forge(ForgeError<GhError>),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Token(Problem::Missing) => write!(
                f,
                "{RUN_TOKEN} is not set: decide-only guard reads GitHub with it"
            ),
            Self::Token(Problem::Invalid) => write!(f, "{RUN_TOKEN} is not a GitHub token"),
            Self::NoGh => f.write_str("no gh on PATH: decide-only guard reads GitHub through gh"),
            Self::Killer(_) => f.write_str("no process-group killer for gh"),
            Self::Gh(_) => f.write_str("gh's scratch configuration could not be prepared"),
            Self::Unavailable => f.write_str("gh could not be set up"),
            Self::Forge(_) => f.write_str("GitHub's answer was refused"),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Token(_) | Self::NoGh | Self::Unavailable => None,
            Self::Killer(source) => Some(source),
            Self::Gh(source) => Some(source),
            Self::Forge(source) => Some(source),
        }
    }
}

/// Runs `assay decide-only` with `args` (`guard`) and `variables`: decides, and for a run or a
/// no-op appends the outputs to `GITHUB_OUTPUT`.
///
/// # Errors
///
/// [`DecideOnlyCliError`] for anything that stops it deciding or writing; nothing is written
/// before the decision.
pub(crate) fn run(
    mut args: impl Iterator<Item = OsString>,
    variables: DecideOnlyVariables,
) -> Result<Ended, DecideOnlyCliError> {
    match (args.next(), args.next()) {
        (Some(step), None) if step == "guard" => {}
        _ => return Err(DecideOnlyCliError::Usage),
    }
    let RunEnv { run_uid, repo } =
        RunEnv::from_vars(&variables.run).map_err(DecideOnlyCliError::Run)?;
    let identity = AppIdentity {
        app: AppId(id(APP_ID, variables.app_id.as_deref())?),
        bot: UserId(id(BOT_ID, variables.bot_id.as_deref())?),
    };
    let event = present(variables.event_name, EVENT_NAME)?
        .to_str()
        .and_then(|name| name.parse::<DecideOnlyEventName>().ok())
        .ok_or(DecideOnlyCliError::Event)?;
    let file = open_payload(&present(variables.event_path, EVENT_PATH)?)
        .map_err(DecideOnlyCliError::PayloadFile)?;
    let payload = DecideOnlyEvent::read(event, file).map_err(DecideOnlyCliError::Payload)?;
    let job = DecideOnlyGuardJob {
        repository_id: run_uid.repo_id,
        app_bot: identity.bot,
    };
    let reads = LazyGitHub::new(variables.reads, repo, identity);
    let guarded = guard_decide_only(job, &payload, &reads);
    let guarded = match (guarded, reads.close()) {
        (Ok(guarded), Ok(())) => guarded,
        (Ok(_), Err(left)) => return Err(DecideOnlyCliError::Left(left)),
        (Err(source), left) => {
            return Err(DecideOnlyCliError::Guard {
                source,
                left: left.err(),
            });
        }
    };
    if let GuardedDecideOnly::Refused(refusal) = guarded {
        return Ok(Ended::Refused(refusal));
    }
    let file =
        open_output(&present(variables.output, OUTPUT)?).map_err(DecideOnlyCliError::OutputFile)?;
    let mut outputs = Outputs::new(file);
    for (name, value) in guarded.outputs() {
        outputs
            .write(name, &value)
            .map_err(DecideOnlyCliError::Output)?;
    }
    Ok(Ended::Written)
}

/// Variable `name`'s value, which must be set.
fn present(value: Option<OsString>, name: &'static str) -> Result<OsString, DecideOnlyCliError> {
    value.ok_or(DecideOnlyCliError::Variable {
        name,
        problem: Problem::Missing,
    })
}

/// Variable `name`'s value as an id.
fn id(name: &'static str, value: Option<&OsStr>) -> Result<NonZeroU64, DecideOnlyCliError> {
    ledger_job::id(value).map_err(|problem| DecideOnlyCliError::Variable { name, problem })
}

/// GitHub through `gh` with the run token, set up on the first read: an event that needs no
/// read reads no token and starts no process.
///
/// Single-threaded interior mutability (OWN-12): the guard's reads take `&self`, and only the
/// first one moves the setup out ([`Cell::take`]) and fills the [`OnceCell`]; a setup that
/// failed is not tried again.
struct LazyGitHub {
    /// What the first read sets up with; taken by it.
    setup: Cell<Option<Setup>>,
    github: OnceCell<GitHub<GhApi<ForgeSupervisor>, ThreadSleep>>,
}

/// What [`LazyGitHub`] sets gh up with.
struct Setup {
    variables: ReadVariables,
    repository: Repository,
    /// The `assay-verdict` App, whose verdict check the poll finds by app id.
    identity: AppIdentity,
}

impl LazyGitHub {
    fn new(variables: ReadVariables, repository: Repository, identity: AppIdentity) -> Self {
        Self {
            setup: Cell::new(Some(Setup {
                variables,
                repository,
                identity,
            })),
            github: OnceCell::new(),
        }
    }

    /// The adapter, set up now if no read has yet.
    fn github(&self) -> Result<&GitHub<GhApi<ForgeSupervisor>, ThreadSleep>, ReadError> {
        if let Some(github) = self.github.get() {
            return Ok(github);
        }
        let Setup {
            variables,
            repository,
            identity,
        } = self.setup.take().ok_or(ReadError::Unavailable)?;
        let token = ledger_job::token(variables.token.as_deref()).map_err(ReadError::Token)?;
        let program = on_path("gh", variables.path.as_ref()).ok_or(ReadError::NoGh)?;
        let killer = GroupKiller::system().map_err(ReadError::Killer)?;
        let gh = GhApi::new(ForgeSupervisor::new(killer), program, token, variables.gh)
            .map_err(ReadError::Gh)?;
        let github = GitHub::new(Api::new(gh, ThreadSleep), repository, identity);
        Ok(self.github.get_or_init(|| github))
    }

    /// Removes gh's scratch directory, if a read set gh up.
    fn close(self) -> io::Result<()> {
        self.github
            .into_inner()
            .map_or(Ok(()), |github| github.into_api().into_transport().close())
    }
}

impl PollForge for LazyGitHub {
    type Error = ReadError;

    fn open_pulls(&self, limit: usize) -> Result<Vec<PolledPull>, ReadError> {
        PollForge::open_pulls(self.github()?, limit).map_err(ReadError::Forge)
    }

    fn verdict_check(
        &self,
        head: &CommitId,
        name: &CheckName,
    ) -> Result<Option<VerdictView>, ReadError> {
        PollForge::verdict_check(self.github()?, head, name).map_err(ReadError::Forge)
    }

    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, ReadError> {
        PollForge::reviews(self.github()?, number).map_err(ReadError::Forge)
    }
}

impl DecideOnlyGuardReads for LazyGitHub {
    fn pull_identity(&self, pr: NonZeroU64) -> Result<PullIdentity, ReadError> {
        self.github()?.pull_identity(pr).map_err(ReadError::Forge)
    }

    fn default_branch(&self) -> Result<String, ReadError> {
        DecideOnlyGuardReads::default_branch(self.github()?).map_err(ReadError::Forge)
    }
}
