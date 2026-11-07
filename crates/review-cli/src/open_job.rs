//! `assay open --config <plugins-toml>` (#54, #60 ST-4, S0): the pipeline's open job.
//!
//! In order: the arguments and the run, refused for any attempt but the first before anything
//! else is read (R3-05, I1); `guard`'s identity from `REVIEW_NEEDS` (I2); the ledger repository
//! from base config's `[assay].ledger_repo` (D3 A); the two tokens and the verdict App's ids.
//! Only then are the ports built: forge's `GitHub` through `gh` with `REVIEW_GITHUB_TOKEN` (the
//! `redkiln-review` App's), and the ledger's files and refs through `gh` with
//! `REVIEW_LEDGER_TOKEN` (the `redkiln-review-ledger` App's), whose lock deletions are pushed from
//! an object-only repository in a scratch directory ([`crate::ledger_job`]).
//! [`review_cli::run_open`] does the rest. A variable's value is never shown.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::num::NonZeroU64;

use review_app::{
    AppId, OpenError, OpenPorts, Opened, ParseLedgerTimeError, PushAppended, PushKind, Stamped,
    Unlock, UserId,
};
use review_cli::outputs::Identity;
use review_cli::{
    JobName, Needs, NeedsError, OpenCommand, OpenJob, OpenRunError, OpenUsage, RunVariables,
    run_open,
};
use review_forge::github::{
    Api, AppIdentity, ForgeError, GhEnvironment, GhError, GhToken, GitHub, LedgerRefsError,
    LedgerRepoError, Repository, ThreadSleep,
};

use crate::doctor::write_chain;
use crate::ledger_job::{
    self, LEDGER_TOKEN, LedgerJob, LedgerPorts, LedgerSetupError, LedgerTransport, Left, NEEDS,
    Problem, Programs, SystemClock, VariableRefusal, ledger_repo,
};
use crate::status_pr::{APP_ID, BOT_ID, TOKEN};

/// The open job, as its refusals name it.
const OPEN: LedgerJob = LedgerJob {
    name: "open",
    purpose: "open records the push in the ledger",
};

/// The environment `assay open` reads, gathered by `main` (ARC-11).
pub(crate) struct OpenVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`NEEDS`]: guard's outputs.
    pub(crate) needs: Option<OsString>,
    /// [`TOKEN`], the `redkiln-review` App's token.
    pub(crate) token: Option<OsString>,
    /// [`LEDGER_TOKEN`].
    pub(crate) ledger_token: Option<OsString>,
    /// [`APP_ID`].
    pub(crate) app_id: Option<OsString>,
    /// [`BOT_ID`].
    pub(crate) bot_id: Option<OsString>,
    /// `PATH`, which finds `gh` (and git, through forge's environment).
    pub(crate) path: Option<OsString>,
    /// What `gh` inherits.
    pub(crate) gh: GhEnvironment,
}

/// What [`run_open`] fails with over the live ports.
type LiveFailure = OpenRunError<
    ForgeError<GhError>,
    OpenError<
        ForgeError<GhError>,
        LedgerRepoError<GhError>,
        LedgerRefsError<GhError>,
        ParseLedgerTimeError,
    >,
>;

/// A variable `assay open` reads besides the run's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenVariable {
    /// [`NEEDS`].
    Needs,
    /// [`TOKEN`].
    Token,
    /// [`LEDGER_TOKEN`].
    LedgerToken,
    /// [`APP_ID`].
    AppId,
    /// [`BOT_ID`].
    BotId,
}

impl OpenVariable {
    /// Its name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Needs => NEEDS,
            Self::Token => TOKEN,
            Self::LedgerToken => LEDGER_TOKEN,
            Self::AppId => APP_ID,
            Self::BotId => BOT_ID,
        }
    }

    /// What it holds, as a refusal says it.
    pub(crate) const fn holds(self) -> &'static str {
        match self {
            Self::Needs => "guard's outputs as ${{ toJSON(needs) }} gives them",
            Self::Token => "the redkiln-review App's installation token",
            Self::LedgerToken => "the redkiln-review-ledger App's installation token",
            Self::AppId => "the redkiln-review App's id, a positive decimal number",
            Self::BotId => "the id of the redkiln-review App's bot user, a positive decimal number",
        }
    }
}

/// Why `assay open` stopped.
#[derive(Debug)]
pub(crate) enum OpenCliError {
    /// The arguments or the run.
    Usage(OpenUsage),
    /// A variable is missing or invalid.
    Variable {
        variable: OpenVariable,
        problem: Problem,
    },
    /// `guard`'s outputs were refused.
    Needs(NeedsError),
    /// Base config, or the ports' setup.
    Setup(LedgerSetupError),
    /// The check could not be opened.
    Open(Box<LiveFailure>),
}

impl fmt::Display for OpenCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(source) => source.fmt(f),
            Self::Variable { variable, problem } => VariableRefusal {
                job: OPEN.name,
                name: variable.name(),
                holds: variable.holds(),
                problem: *problem,
            }
            .fmt(f),
            Self::Needs(source) => {
                write!(f, "open: {NEEDS}: ")?;
                write_chain(f, source)
            }
            Self::Setup(source) => source.fmt(f),
            Self::Open(source) => {
                f.write_str("open: ")?;
                write_chain(f, source.as_ref())
            }
        }
    }
}

/// How `assay open` ended: the check opened, as one line, or why it was not.
pub(crate) type Ended = ledger_job::Ended<OpenCliError>;

/// What open reads before it builds a port.
struct Prepared {
    command: OpenCommand,
    identity: Identity,
    ledger: Repository,
    token: GhToken,
    ledger_token: GhToken,
    app: AppIdentity,
}

/// Runs `assay open` with `args` and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: OpenVariables) -> Ended {
    let mut left = Vec::new();
    let line =
        prepare(args, &variables).and_then(|prepared| connect(prepared, variables, &mut left));
    Ended { line, left }
}

/// Everything open reads before a port exists, the run first (I1).
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &OpenVariables,
) -> Result<Prepared, OpenCliError> {
    let command = OpenCommand::parse(args, &variables.run).map_err(OpenCliError::Usage)?;
    let needs = variables
        .needs
        .as_ref()
        .ok_or_else(|| refused(OpenVariable::Needs)(Problem::Missing))?;
    let needs =
        Needs::parse(needs.as_encoded_bytes(), &[JobName::Guard]).map_err(OpenCliError::Needs)?;
    let identity = Identity::from_needs(&needs).map_err(OpenCliError::Needs)?;
    let ledger = ledger_repo(OPEN, &command.config).map_err(OpenCliError::Setup)?;
    Ok(Prepared {
        command,
        identity,
        ledger,
        token: ledger_job::token(variables.token.as_deref())
            .map_err(refused(OpenVariable::Token))?,
        ledger_token: ledger_job::token(variables.ledger_token.as_deref())
            .map_err(refused(OpenVariable::LedgerToken))?,
        app: AppIdentity {
            app: AppId(id(OpenVariable::AppId, variables.app_id.as_deref())?),
            bot: UserId(id(OpenVariable::BotId, variables.bot_id.as_deref())?),
        },
    })
}

/// Refuses `variable` with a problem.
fn refused(variable: OpenVariable) -> impl Fn(Problem) -> OpenCliError {
    move |problem| OpenCliError::Variable { variable, problem }
}

/// Builds the ports in a scratch directory, opens the check, and removes the directories,
/// pushing onto `left` each one that could not be removed.
fn connect(
    prepared: Prepared,
    variables: OpenVariables,
    left: &mut Vec<Left>,
) -> Result<String, OpenCliError> {
    let Prepared {
        command,
        identity,
        ledger,
        token,
        ledger_token,
        app,
    } = prepared;
    let programs = Programs {
        path: variables.path,
        gh: variables.gh,
    };
    LedgerTransport::open(OPEN, (ledger, ledger_token), programs, left)
        .map_err(OpenCliError::Setup)?
        .run(left, |ports, left| {
            let job = OpenJob {
                run: &command.run,
                identity: &identity,
            };
            open_check(job, (token, app), &ports, left)
        })
}

/// Opens the check with the App's transport, authenticated as `token`, closing it after.
fn open_check(
    job: OpenJob<'_>,
    (token, app): (GhToken, AppIdentity),
    ledger: &LedgerPorts<'_, '_>,
    left: &mut Vec<Left>,
) -> Result<String, OpenCliError> {
    let app_gh = ledger.gh.transport(token).map_err(OpenCliError::Setup)?;
    // The adapter stores the repository; the command keeps it for the job.
    let github = GitHub::new(
        Api::new(app_gh, ThreadSleep),
        job.run.run_env().repo.clone(),
        app,
    );
    let ports = OpenPorts {
        forge: &github,
        ledger: ledger.files,
        refs: ledger.refs,
        clock: &SystemClock,
    };
    let opened = run_open(job, &github, &ports)
        .map(|opened| line(&opened))
        .map_err(|error| OpenCliError::Open(Box::new(error)));
    if let Err(error) = github.into_api().into_transport().close() {
        left.push(Left::Gh(TOKEN, error));
    }
    opened
}

/// What open did, as one line for the job's log.
fn line(opened: &Opened<ForgeError<GhError>, LedgerRefsError<GhError>>) -> String {
    let stamped = match &opened.stamped {
        Stamped::Created => "created in_progress",
        Stamped::Restamped => "restamped in_progress",
        Stamped::Unchanged => "already in_progress with this stamp",
        Stamped::Reopened => "reopened in_progress",
        Stamped::ReDeciding(_) => "not reopened, so published failure (re-deciding)",
        Stamped::Yielded { .. } => "carries a newer run's stamp, so this run yields",
    };
    let record = match &opened.push {
        None => "no ledger record".to_owned(),
        Some(appended) => {
            let record = appended.record();
            let kind = match record.push.kind {
                PushKind::Opener => "opener",
                PushKind::Push { .. } => "push",
            };
            let again = match appended {
                PushAppended::Recorded(_) => "",
                PushAppended::AlreadyRecorded(_) => " (already recorded)",
            };
            format!("{kind} record {}{again}", record.seq)
        }
    };
    let unlock = match &opened.unlock {
        Unlock::NotTaken | Unlock::Released => String::new(),
        Unlock::Failed(error) => format!(
            "; the publish lock was not released and expires with its lease: {}",
            Chain(error)
        ),
    };
    format!(
        "assay open: {} check {} {stamped}, stamp {}; {record}{unlock}",
        opened.name, opened.check.0, opened.stamp
    )
}

/// An error with each of its causes, joined by `: `.
struct Chain<'e>(&'e dyn std::error::Error);

impl fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_chain(f, self.0)
    }
}

/// `variable`'s `value` as an id in canonical decimal.
fn id(variable: OpenVariable, value: Option<&OsStr>) -> Result<NonZeroU64, OpenCliError> {
    ledger_job::id(value).map_err(refused(variable))
}
