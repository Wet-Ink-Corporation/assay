//! `assay open --config <plugins-toml>` (#54, #60 ST-4, S0): the pipeline's open job.
//!
//! In order: the arguments and the run, refused for any attempt but the first before anything
//! else is read (R3-05, I1); `guard`'s identity from `ASSAY_NEEDS` (I2); the ledger repository
//! from base config's `[assay].ledger_repo` (D3 A); the two tokens and the verdict App's ids.
//! Only then are the ports built: forge's `GitHub` through `gh` with `ASSAY_GITHUB_TOKEN` (the
//! `assay-verdict` App's), and the ledger's files and refs through `gh` with
//! `ASSAY_LEDGER_TOKEN` (the `assay-ledger-writer` App's), whose lock deletions are pushed from
//! an object-only repository in a scratch directory. [`assay_cli::run_open`] does the rest.
//! A variable's value is never shown.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime};

use assay_app::{
    AppId, LedgerTime, OpenClock, OpenError, OpenPorts, Opened, ParseLedgerTimeError, PushAppended,
    PushKind, Stamped, Unlock, UserId,
};
use assay_cli::outputs::Identity;
use assay_cli::{
    ForgeSupervisor, JobName, Needs, NeedsError, OpenCommand, OpenJob, OpenRunError, OpenUsage,
    RunVariables, run_open,
};
use assay_forge::git::{GitEnvironment, GitError, GitLimits, ObjectRepo, PushRemote};
use assay_forge::github::{
    Api, AppIdentity, ForgeError, GhApi, GhEnvironment, GhError, GhSetupError, GhToken, GitHub,
    LedgerRefsError, LedgerRefsRepo, LedgerRemoteError, LedgerRepo, LedgerRepoError, Repository,
    ThreadSleep, ledger_push_remote,
};
use assay_forge::process::{GroupKiller, KillerError};

use crate::check::on_path;
use crate::doctor::{self, DoctorCliError, write_chain};
use crate::status_pr::{APP_ID, BOT_ID, TOKEN};

/// `guard`'s outputs, `${{ toJSON(needs) }}`.
pub(crate) const NEEDS: &str = "ASSAY_NEEDS";
/// The `assay-ledger-writer` App's installation token.
pub(crate) const LEDGER_TOKEN: &str = "ASSAY_LEDGER_TOKEN";

/// A lock deletion's push: a few refs to one remote, well inside the lock's lease (H-13).
const PUSH_LIMITS: GitLimits = GitLimits {
    timeout: Duration::from_secs(60),
    max_output_bytes: 1 << 20,
};

/// The environment `assay open` reads, gathered by `main` (ARC-11).
pub(crate) struct OpenVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`NEEDS`].
    pub(crate) needs: Option<OsString>,
    /// [`TOKEN`], the `assay-verdict` App's token.
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

/// What is wrong with a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Problem {
    /// It is not set.
    Missing,
    /// It holds something it may not; its value is never shown.
    Invalid,
}

/// `gh` through the shared supervisor.
type Gh = GhApi<ForgeSupervisor>;
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

/// Why `assay open` stopped.
#[derive(Debug)]
pub(crate) enum OpenCliError {
    /// The arguments or the run.
    Usage(OpenUsage),
    /// A variable is missing or invalid.
    Variable {
        name: &'static str,
        problem: Problem,
    },
    /// `guard`'s outputs were refused.
    Needs(NeedsError),
    /// Base config could not be read or was refused.
    Config(DoctorCliError),
    /// Base config names no ledger repository.
    NoLedgerRepo,
    /// `[assay].ledger_repo` is not `owner/name`.
    LedgerRepo,
    /// No `gh` on `PATH`.
    NoGh,
    /// No process-group killer for `gh` and git.
    Killer(KillerError),
    /// gh's scratch configuration could not be prepared.
    Gh(GhSetupError),
    /// The scratch directory could not be made.
    Scratch(io::Error),
    /// The object-only repository lock deletions push from could not be made.
    Git(GitError),
    /// The ledger's push remote could not be built.
    Remote(LedgerRemoteError),
    /// The check could not be opened.
    Open(Box<LiveFailure>),
}

impl fmt::Display for OpenCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(source) => source.fmt(f),
            Self::Variable { name, problem } => {
                let holds = match *name {
                    NEEDS => "guard's outputs as ${{ toJSON(needs) }} gives them",
                    TOKEN => "the assay-verdict App's installation token",
                    LEDGER_TOKEN => "the assay-ledger-writer App's installation token",
                    APP_ID => "the assay-verdict App's id, a positive decimal number",
                    _ => "the id of the assay-verdict App's bot user, a positive decimal number",
                };
                match problem {
                    Problem::Missing => write!(f, "open: {name} is not set: it holds {holds}"),
                    Problem::Invalid => write!(f, "open: {name} is not {holds}"),
                }
            }
            Self::Needs(source) => {
                write!(f, "open: {NEEDS}: ")?;
                write_chain(f, source)
            }
            Self::Config(source) => write!(f, "open: {source}"),
            Self::NoLedgerRepo => f.write_str(
                "open: base config sets no [assay].ledger_repo: open records the push in the \
                 ledger",
            ),
            Self::LedgerRepo => {
                f.write_str("open: base config's [assay].ledger_repo is not owner/name")
            }
            Self::NoGh => f.write_str("open: no gh on PATH: open reaches GitHub through gh"),
            Self::Killer(source) => {
                f.write_str("open: ")?;
                write_chain(f, source)
            }
            Self::Gh(source) => {
                f.write_str("open: ")?;
                write_chain(f, source)
            }
            Self::Scratch(source) => write!(f, "open: the scratch directory: {source}"),
            Self::Git(source) => {
                f.write_str("open: the ledger's push repository: ")?;
                write_chain(f, source)
            }
            Self::Remote(source) => {
                f.write_str("open: the ledger's push remote: ")?;
                write_chain(f, source)
            }
            Self::Open(source) => {
                f.write_str("open: ")?;
                write_chain(f, source.as_ref())
            }
        }
    }
}

/// A scratch directory that could not be removed.
#[derive(Debug)]
pub(crate) enum Left {
    /// gh's scratch configuration directory, of the named token's transport.
    Gh(&'static str, io::Error),
    /// The ledger push repository's scratch directory.
    Scratch(io::Error),
}

impl fmt::Display for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gh(token, source) => write!(
                f,
                "could not remove gh's scratch configuration directory ({token}): {source}"
            ),
            Self::Scratch(source) => {
                write!(f, "could not remove open's scratch directory: {source}")
            }
        }
    }
}

/// How `assay open` ended: the line to print or why there is none, and what was left behind.
pub(crate) struct Ended {
    /// The check opened, as one line.
    pub(crate) opened: Result<String, OpenCliError>,
    /// The scratch directories that could not be removed.
    pub(crate) left: Vec<Left>,
}

/// The system clock for the lock's lease, and a real pause between lock attempts.
struct SystemClock;

impl OpenClock for SystemClock {
    type Error = ParseLedgerTimeError;

    fn now(&self) -> Result<LedgerTime, ParseLedgerTimeError> {
        LedgerTime::from_system_time(SystemTime::now())
    }

    /// 2, 4 and then 8 seconds: 14 in all, well under the lock's 2-minute TTL.
    fn pause(&self, retry: u8) {
        let seconds = match retry {
            0 | 1 => 2,
            2 => 4,
            _ => 8,
        };
        thread::sleep(Duration::from_secs(seconds));
    }
}

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
    let opened =
        prepare(args, &variables).and_then(|prepared| connect(prepared, variables, &mut left));
    Ended { opened, left }
}

/// Everything open reads before a port exists, the run first (I1).
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &OpenVariables,
) -> Result<Prepared, OpenCliError> {
    let command = OpenCommand::parse(args, &variables.run).map_err(OpenCliError::Usage)?;
    let needs = variables.needs.as_ref().ok_or(OpenCliError::Variable {
        name: NEEDS,
        problem: Problem::Missing,
    })?;
    let needs =
        Needs::parse(needs.as_encoded_bytes(), &[JobName::Guard]).map_err(OpenCliError::Needs)?;
    let identity = Identity::from_needs(&needs).map_err(OpenCliError::Needs)?;
    let config = doctor::read_pins(&command.config).map_err(OpenCliError::Config)?;
    let ledger = config
        .assay()
        .ledger_repo
        .as_deref()
        .ok_or(OpenCliError::NoLedgerRepo)?
        .parse()
        .map_err(|_not_owner_name| OpenCliError::LedgerRepo)?;
    Ok(Prepared {
        command,
        identity,
        ledger,
        token: token(TOKEN, variables.token.as_ref())?,
        ledger_token: token(LEDGER_TOKEN, variables.ledger_token.as_ref())?,
        app: AppIdentity {
            app: AppId(id(APP_ID, variables.app_id.as_ref())?),
            bot: UserId(id(BOT_ID, variables.bot_id.as_ref())?),
        },
    })
}

/// How `gh` runs, for both transports.
struct GhSetup {
    supervisor: ForgeSupervisor,
    program: PathBuf,
    environment: GhEnvironment,
}

impl GhSetup {
    /// A transport authenticated as `token`.
    fn transport(&self, token: GhToken) -> Result<Gh, OpenCliError> {
        // Each transport stores its own handle, program and environment.
        GhApi::new(
            self.supervisor.clone(),
            self.program.clone(),
            token,
            self.environment.clone(),
        )
        .map_err(OpenCliError::Gh)
    }
}

/// Builds the ports in a scratch directory, opens the check, and removes the directories,
/// pushing onto `left` each one that could not be removed.
fn connect(
    prepared: Prepared,
    variables: OpenVariables,
    left: &mut Vec<Left>,
) -> Result<String, OpenCliError> {
    let program = on_path("gh", variables.path.as_ref()).ok_or(OpenCliError::NoGh)?;
    let path = variables.path.ok_or(OpenCliError::NoGh)?;
    let killer = GroupKiller::system().map_err(OpenCliError::Killer)?;
    let remote = ledger_push_remote(&prepared.ledger, &prepared.ledger_token)
        .map_err(OpenCliError::Remote)?;
    let scratch = tempfile::tempdir().map_err(OpenCliError::Scratch)?;
    // The repository and the supervisor each keep a handle on the killer.
    let pusher = ObjectRepo::init(
        scratch.path().join("ledger.git"),
        GitEnvironment::new(path, killer.clone()),
        PUSH_LIMITS,
    )
    .map_err(OpenCliError::Git);
    let gh = GhSetup {
        supervisor: ForgeSupervisor::new(killer),
        program,
        environment: variables.gh,
    };
    let opened = pusher.and_then(|pusher| with_ledger(prepared, &gh, (pusher, remote), left));
    if let Err(error) = scratch.close() {
        left.push(Left::Scratch(error));
    }
    opened
}

/// Opens the check with the ledger's transport, closing it after.
fn with_ledger(
    prepared: Prepared,
    gh: &GhSetup,
    (pusher, remote): (ObjectRepo, PushRemote),
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
    let ledger_gh = gh.transport(ledger_token)?;
    let opened = gh.transport(token).and_then(|app_gh| {
        // The adapter stores the repository; the command keeps it for the job.
        let github = GitHub::new(
            Api::new(app_gh, ThreadSleep),
            command.run.run_env().repo.clone(),
            app,
        );
        // The file and ref adapters each store the ledger repository.
        let files = LedgerRepo::new(Api::new(&ledger_gh, ThreadSleep), ledger.clone());
        let refs = LedgerRefsRepo::new(Api::new(&ledger_gh, ThreadSleep), ledger, pusher, remote);
        let job = OpenJob {
            run: &command.run,
            identity: &identity,
        };
        let ports = OpenPorts {
            forge: &github,
            ledger: &files,
            refs: &refs,
            clock: &SystemClock,
        };
        let opened = run_open(job, &github, &ports)
            .map(|opened| line(&opened))
            .map_err(|error| OpenCliError::Open(Box::new(error)));
        if let Err(error) = github.into_api().into_transport().close() {
            left.push(Left::Gh(TOKEN, error));
        }
        opened
    });
    if let Err(error) = ledger_gh.close() {
        left.push(Left::Gh(LEDGER_TOKEN, error));
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

/// Variable `name` as a token; its value is never shown.
fn token(name: &'static str, value: Option<&OsString>) -> Result<GhToken, OpenCliError> {
    let refused = |problem| OpenCliError::Variable { name, problem };
    let text = value
        .ok_or_else(|| refused(Problem::Missing))?
        .to_str()
        .ok_or_else(|| refused(Problem::Invalid))?;
    GhToken::new(text).map_err(|_invalid| refused(Problem::Invalid))
}

/// Variable `name` as an id in canonical decimal.
fn id(name: &'static str, value: Option<&OsString>) -> Result<NonZeroU64, OpenCliError> {
    let refused = |problem| OpenCliError::Variable { name, problem };
    value
        .ok_or_else(|| refused(Problem::Missing))?
        .to_str()
        .and_then(canonical_id)
        .ok_or_else(|| refused(Problem::Invalid))
}

/// `text` as a positive number: ASCII digits, the first not `0`, within `u64`.
fn canonical_id(text: &str) -> Option<NonZeroU64> {
    let canonical = text
        .bytes()
        .next()
        .is_some_and(|first| matches!(first, b'1'..=b'9'))
        && text.bytes().all(|byte| byte.is_ascii_digit());
    if canonical { text.parse().ok() } else { None }
}
