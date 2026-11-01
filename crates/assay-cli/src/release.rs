//! `assay release --config <plugins-toml>` (#60 ST-10, S10): the pipeline's release job.
//!
//! In order: the arguments; the run, refused for any attempt but the first before a token is
//! read (R3-05, I1); `admit`'s and `review`'s outputs from `ASSAY_NEEDS`. When `admit` wrote no
//! `slot` there is nothing to settle and the job ends successfully with no config, token or port
//! read (beside the job's own `if:`). Otherwise the ledger repository from base config's
//! `[assay].ledger_repo` (D3 A) and `ASSAY_LEDGER_TOKEN`, the `assay-ledger-writer` App's
//! installation token; only then are the ports built: the ledger's files and refs through `gh`
//! with that token, the slot's deletion pushed from an object-only repository in a scratch
//! directory ([`crate::ledger_job`]). [`assay_app::release`] does the rest. A variable's value
//! is never shown.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

use assay_app::{
    Appended, LedgerTime, ParseLedgerTimeError, ReleaseError, ReleasePorts, ReleaseRequest,
    Released, ReviewCost, Settled,
};
use assay_cli::outputs::{HeldSlot, ReviewEnd};
use assay_cli::{FirstAttempt, JobName, Needs, NeedsError, RunEnv, RunEnvError, RunVariables};
use assay_forge::github::{
    GhEnvironment, GhError, GhToken, LedgerRefsError, LedgerRepoError, Repository,
};

use crate::doctor::write_chain;
use crate::ledger_job::{
    self, LEDGER_TOKEN, LedgerJob, LedgerSetupError, LedgerTransport, Left, NEEDS, Problem,
    Programs, VariableRefusal, ledger_repo,
};

/// The release job, as its refusals name it.
const RELEASE: LedgerJob = LedgerJob {
    name: "release",
    purpose: "release settles the run in the ledger",
};

/// The one flag `release` takes.
const CONFIG: &str = "--config";

/// The jobs `release` needs (04 §1 S10).
const NEEDED: [JobName; 2] = [JobName::Admit, JobName::Review];

/// The environment `assay release` reads, gathered by `main` (ARC-11).
pub(crate) struct ReleaseVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`NEEDS`]: `admit`'s and `review`'s outputs.
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
    /// Base config, or the ports' setup.
    Setup(LedgerSetupError),
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
            Self::Variable { variable, problem } => VariableRefusal {
                job: RELEASE.name,
                name: variable.name(),
                holds: variable.holds(),
                problem: *problem,
            }
            .fmt(f),
            Self::Needs(source) => {
                write!(f, "release: {NEEDS}: ")?;
                write_chain(f, source)
            }
            Self::Setup(source) => source.fmt(f),
            Self::Clock(source) => write!(f, "release: the clock: {source}"),
            Self::Release(source) => {
                f.write_str("release: ")?;
                write_chain(f, source.as_ref())
            }
        }
    }
}

/// How `assay release` ended: what release did, as one line, or why it did not.
pub(crate) type Ended = ledger_job::Ended<ReleaseCliError>;

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
    let line = match prepare(args, &variables) {
        Ok(Some(prepared)) => connect(prepared, variables, &mut left),
        Ok(None) => Ok(
            "release: admit holds no slot for this run, so there is nothing to settle".to_owned(),
        ),
        Err(error) => Err(error),
    };
    Ended { line, left }
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
    let review = ReviewCost::from(ReviewEnd::from_needs(&needs).map_err(ReleaseCliError::Needs)?);
    let ledger = ledger_repo(RELEASE, &config).map_err(ReleaseCliError::Setup)?;
    let token = ledger_job::token(variables.ledger_token.as_deref()).map_err(|problem| {
        ReleaseCliError::Variable {
            variable: Variable::LedgerToken,
            problem,
        }
    })?;
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
    let programs = Programs {
        path: variables.path,
        gh: variables.gh,
    };
    let request = ReleaseRequest {
        run_uid: run.run_env().run_uid,
        slot: held.slot,
        lease_commit: &held.lease_commit,
        review,
        now,
    };
    LedgerTransport::open(RELEASE, (ledger, token), programs, left)
        .map_err(ReleaseCliError::Setup)?
        .run(left, |ports, _left| {
            assay_app::release(
                &ReleasePorts {
                    ledger: ports.files,
                    refs: ports.refs,
                },
                &request,
            )
            .map(|settled| line(&request, &settled))
            .map_err(|error| ReleaseCliError::Release(Box::new(error)))
        })
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
