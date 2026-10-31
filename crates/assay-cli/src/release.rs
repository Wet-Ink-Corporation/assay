//! `assay release --config <plugins-toml>` (#60 ST-10, S10): the pipeline's release job.
//!
//! In order: the arguments; the run, refused for any attempt but the first before a token is
//! read (R3-05, I1); `admit`'s and `review`'s outputs from `ASSAY_NEEDS`. When `admit` wrote no
//! `slot` there is nothing to settle and the job ends successfully with no config, token or port
//! read (beside the job's own `if:`). Otherwise the ledger repository from base config's
//! `[assay].ledger_repo` (D3 A) and `ASSAY_LEDGER_TOKEN`, the `assay-ledger-writer` App's
//! installation token; only then are the ports built: the ledger's files and refs through `gh`
//! with that token, the slot's deletion pushed from an object-only repository in a scratch
//! directory. [`assay_app::release`] does the rest. A variable's value is never shown.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use assay_app::{
    Appended, LedgerTime, ParseLedgerTimeError, ReleaseError, ReleasePorts, ReleaseRequest,
    Released, ReviewCost, Settled,
};
use assay_cli::outputs::{HeldSlot, ReviewEnd};
use assay_cli::{
    FirstAttempt, ForgeSupervisor, JobName, Needs, NeedsError, RunEnv, RunEnvError, RunVariables,
};
use assay_forge::git::{GitEnvironment, GitError, GitLimits, ObjectRepo, PushRemote};
use assay_forge::github::{
    Api, GhApi, GhEnvironment, GhError, GhSetupError, GhToken, LedgerRefsError, LedgerRefsRepo,
    LedgerRemoteError, LedgerRepo, LedgerRepoError, Repository, ThreadSleep, ledger_push_remote,
};
use assay_forge::process::{GroupKiller, KillerError};

use crate::check::on_path;
use crate::doctor::{self, DoctorCliError, write_chain};

/// `admit`'s and `review`'s outputs, `${{ toJSON(needs) }}`.
pub(crate) const NEEDS: &str = "ASSAY_NEEDS";
/// The `assay-ledger-writer` App's installation token.
pub(crate) const LEDGER_TOKEN: &str = "ASSAY_LEDGER_TOKEN";

/// The one flag `release` takes.
const CONFIG: &str = "--config";

/// The jobs `release` needs (04 §1 S10).
const NEEDED: [JobName; 2] = [JobName::Admit, JobName::Review];

/// A slot deletion's push: one ref to one remote (H-13).
const PUSH_LIMITS: GitLimits = GitLimits {
    timeout: Duration::from_secs(60),
    max_output_bytes: 1 << 20,
};

/// The environment `assay release` reads, gathered by `main` (ARC-11).
pub(crate) struct ReleaseVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`NEEDS`].
    pub(crate) needs: Option<OsString>,
    /// [`LEDGER_TOKEN`].
    pub(crate) ledger_token: Option<OsString>,
    /// `PATH`, which finds `gh` (and git, through forge's environment).
    pub(crate) path: Option<OsString>,
    /// What `gh` inherits.
    pub(crate) gh: GhEnvironment,
    /// The wall clock when the command started: the `final` record's time.
    pub(crate) now: SystemTime,
}

/// A variable `release` reads besides the run's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variable {
    /// [`NEEDS`].
    Needs,
    /// [`LEDGER_TOKEN`].
    LedgerToken,
}

impl Variable {
    /// The variable's name.
    const fn name(self) -> &'static str {
        match self {
            Self::Needs => NEEDS,
            Self::LedgerToken => LEDGER_TOKEN,
        }
    }

    /// What it holds.
    const fn holds(self) -> &'static str {
        match self {
            Self::Needs => "admit's and review's outputs as ${{ toJSON(needs) }} gives them",
            Self::LedgerToken => "the assay-ledger-writer App's installation token",
        }
    }
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
/// What [`assay_app::release`] fails with over the live ports.
type LiveFailure = ReleaseError<LedgerRepoError<GhError>, LedgerRefsError<GhError>>;

/// Why `assay release` stopped.
#[derive(Debug)]
pub(crate) enum ReleaseCliError {
    /// The arguments are not exactly `--config <plugins-toml>`.
    Usage,
    /// The run could not be read, or is not its first attempt.
    Run(RunEnvError),
    /// A variable is missing or invalid.
    Variable {
        variable: Variable,
        problem: Problem,
    },
    /// The upstream jobs' outputs were refused.
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
    /// The object-only repository the slot's deletion pushes from could not be made.
    Git(GitError),
    /// The ledger's push remote could not be built.
    Remote(LedgerRemoteError),
    /// The clock reads outside 1970 to 9999.
    Clock(ParseLedgerTimeError),
    /// The run could not be settled.
    Release(Box<LiveFailure>),
}

impl fmt::Display for ReleaseCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(f, "usage: assay release {CONFIG} <plugins-toml>"),
            Self::Run(source) => {
                f.write_str("release: ")?;
                write_chain(f, source)
            }
            Self::Variable { variable, problem } => {
                let (name, holds) = (variable.name(), variable.holds());
                match problem {
                    Problem::Missing => write!(f, "release: {name} is not set: it holds {holds}"),
                    Problem::Invalid => write!(f, "release: {name} is not {holds}"),
                }
            }
            Self::Needs(source) => {
                write!(f, "release: {NEEDS}: ")?;
                write_chain(f, source)
            }
            Self::Config(source) => write!(f, "release: {source}"),
            Self::NoLedgerRepo => f.write_str(
                "release: base config sets no [assay].ledger_repo: release settles the run in the \
                 ledger",
            ),
            Self::LedgerRepo => {
                f.write_str("release: base config's [assay].ledger_repo is not owner/name")
            }
            Self::NoGh => f.write_str("release: no gh on PATH: release reaches GitHub through gh"),
            Self::Killer(source) => {
                f.write_str("release: ")?;
                write_chain(f, source)
            }
            Self::Gh(source) => {
                f.write_str("release: ")?;
                write_chain(f, source)
            }
            Self::Scratch(source) => write!(f, "release: the scratch directory: {source}"),
            Self::Git(source) => {
                f.write_str("release: the ledger's push repository: ")?;
                write_chain(f, source)
            }
            Self::Remote(source) => {
                f.write_str("release: the ledger's push remote: ")?;
                write_chain(f, source)
            }
            Self::Clock(source) => write!(f, "release: the clock: {source}"),
            Self::Release(source) => {
                f.write_str("release: ")?;
                write_chain(f, source.as_ref())
            }
        }
    }
}

/// A scratch directory that could not be removed.
#[derive(Debug)]
pub(crate) enum Left {
    /// gh's scratch configuration directory.
    Gh(io::Error),
    /// The ledger push repository's scratch directory.
    Scratch(io::Error),
}

impl fmt::Display for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gh(source) => {
                write!(
                    f,
                    "could not remove gh's scratch configuration directory: {source}"
                )
            }
            Self::Scratch(source) => {
                write!(f, "could not remove release's scratch directory: {source}")
            }
        }
    }
}

/// How `assay release` ended: the line to print or why there is none, and what was left behind.
pub(crate) struct Ended {
    /// What release did, as one line.
    pub(crate) released: Result<String, ReleaseCliError>,
    /// The scratch directories that could not be removed.
    pub(crate) left: Vec<Left>,
}

/// What release settles, read before any port exists.
struct Prepared {
    run: FirstAttempt,
    held: HeldSlot,
    review: ReviewCost,
    ledger: Repository,
    token: GhToken,
    /// When the `final` record is written.
    now: LedgerTime,
}

/// Runs `assay release` with `args` and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: ReleaseVariables) -> Ended {
    let mut left = Vec::new();
    let released = match prepare(args, &variables) {
        Ok(Some(prepared)) => connect(prepared, variables, &mut left),
        Ok(None) => Ok(
            "release: admit holds no slot for this run, so there is nothing to settle".to_owned(),
        ),
        Err(error) => Err(error),
    };
    Ended { released, left }
}

/// Everything release reads before a port exists, the run first (I1); `None` when admit holds no
/// slot.
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &ReleaseVariables,
) -> Result<Option<Prepared>, ReleaseCliError> {
    let config = config_path(args)?;
    let run = RunEnv::from_vars(&variables.run)
        .and_then(|run| run.first_attempt().map_err(RunEnvError::from))
        .map_err(ReleaseCliError::Run)?;
    let needs = variables.needs.as_ref().ok_or(ReleaseCliError::Variable {
        variable: Variable::Needs,
        problem: Problem::Missing,
    })?;
    let needs = Needs::parse(needs.as_encoded_bytes(), &NEEDED).map_err(ReleaseCliError::Needs)?;
    let Some(held) = HeldSlot::from_needs(&needs).map_err(ReleaseCliError::Needs)? else {
        return Ok(None);
    };
    let review = match ReviewEnd::from_needs(&needs).map_err(ReleaseCliError::Needs)? {
        ReviewEnd::Reported(report) => ReviewCost::Reported(report.cost),
        ReviewEnd::Failed | ReviewEnd::Skipped => ReviewCost::Died,
        ReviewEnd::Cancelled => ReviewCost::Cancelled,
    };
    let ledger = doctor::read_pins(&config)
        .map_err(ReleaseCliError::Config)?
        .assay()
        .ledger_repo
        .as_deref()
        .ok_or(ReleaseCliError::NoLedgerRepo)?
        .parse()
        .map_err(|_not_owner_name| ReleaseCliError::LedgerRepo)?;
    let token = token(variables.ledger_token.as_ref())?;
    let now = LedgerTime::from_system_time(variables.now).map_err(ReleaseCliError::Clock)?;
    Ok(Some(Prepared {
        run,
        held,
        review,
        ledger,
        token,
        now,
    }))
}

/// The path after exactly `--config`.
fn config_path(mut args: impl Iterator<Item = OsString>) -> Result<PathBuf, ReleaseCliError> {
    match (args.next(), args.next(), args.next()) {
        (Some(flag), Some(path), None) if flag == CONFIG => Ok(PathBuf::from(path)),
        _ => Err(ReleaseCliError::Usage),
    }
}

/// [`LEDGER_TOKEN`] as a token gh may carry.
fn token(value: Option<&OsString>) -> Result<GhToken, ReleaseCliError> {
    let invalid = |problem| ReleaseCliError::Variable {
        variable: Variable::LedgerToken,
        problem,
    };
    let value = value.ok_or_else(|| invalid(Problem::Missing))?;
    let text = value.to_str().ok_or_else(|| invalid(Problem::Invalid))?;
    GhToken::new(text).map_err(|_invalid| invalid(Problem::Invalid))
}

/// Builds the ports in a scratch directory, settles the run, and removes the directories,
/// pushing onto `left` each one that could not be removed.
fn connect(
    prepared: Prepared,
    variables: ReleaseVariables,
    left: &mut Vec<Left>,
) -> Result<String, ReleaseCliError> {
    let Prepared {
        run,
        held,
        review,
        ledger,
        token,
        now,
    } = prepared;
    let program = on_path("gh", variables.path.as_ref()).ok_or(ReleaseCliError::NoGh)?;
    let path = variables.path.ok_or(ReleaseCliError::NoGh)?;
    let killer = GroupKiller::system().map_err(ReleaseCliError::Killer)?;
    let remote = ledger_push_remote(&ledger, &token).map_err(ReleaseCliError::Remote)?;
    let scratch = tempfile::tempdir().map_err(ReleaseCliError::Scratch)?;
    // The repository and the supervisor each keep a handle on the killer.
    let pusher = ObjectRepo::init(
        scratch.path().join("ledger.git"),
        GitEnvironment::new(path, killer.clone()),
        PUSH_LIMITS,
    )
    .map_err(ReleaseCliError::Git);
    let released = pusher.and_then(|pusher| {
        GhApi::new(ForgeSupervisor::new(killer), program, token, variables.gh)
            .map_err(ReleaseCliError::Gh)
            .and_then(|gh| {
                let request = ReleaseRequest {
                    run_uid: run.run_env().run_uid,
                    slot: held.slot,
                    lease_commit: &held.lease_commit,
                    review,
                    now,
                };
                let settled = settle(&gh, ledger, (pusher, remote), &request);
                if let Err(error) = gh.close() {
                    left.push(Left::Gh(error));
                }
                settled
            })
    });
    if let Err(error) = scratch.close() {
        left.push(Left::Scratch(error));
    }
    released
}

/// Settles `request`'s run in `ledger` over its transport `gh`.
fn settle(
    gh: &Gh,
    ledger: Repository,
    (pusher, remote): (ObjectRepo, PushRemote),
    request: &ReleaseRequest<'_>,
) -> Result<String, ReleaseCliError> {
    // The file and ref adapters each store the ledger repository.
    let files = LedgerRepo::new(Api::new(gh, ThreadSleep), ledger.clone());
    let refs = LedgerRefsRepo::new(Api::new(gh, ThreadSleep), ledger, pusher, remote);
    assay_app::release(
        &ReleasePorts {
            ledger: &files,
            refs: &refs,
        },
        request,
    )
    .map(|settled| line(request, &settled))
    .map_err(|error| ReleaseCliError::Release(Box::new(error)))
}

/// What release did, as one line for the job's log.
fn line(request: &ReleaseRequest<'_>, settled: &Settled) -> String {
    let slot = match settled.slot {
        Released::Released => format!("slot {} released", request.slot),
        Released::NotHeld => format!(
            "slot {} no longer held this run's lease (reaped), so nothing was deleted",
            request.slot
        ),
    };
    let cost = match &settled.cost {
        Appended::Recorded => format!("final cost {} recorded", settled.record.aeu),
        Appended::AlreadyRecorded(found) => format!(
            "a final cost of {} was already recorded, so nothing was written",
            found.aeu
        ),
    };
    format!("release: {slot}; {cost}")
}
