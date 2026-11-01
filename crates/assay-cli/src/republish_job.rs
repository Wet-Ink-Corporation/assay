//! `assay decide-only publish --config <plugins-toml> --state <dir>` (#61 DO-12, D9 A): the
//! redecide job's last step, after `upload-artifact`.
//!
//! In order, everything it can refuse before a token is read: its flags; the run, refused for
//! any attempt but the first (J2, R3-05); guard's outputs from `ASSAY_NEEDS` (`guard` alone);
//! `ASSAY_PR`, which must be one of guard's `prs`; the upload step's outcome from
//! `ASSAY_PERSIST`, exactly one of GitHub's four words; rubric v1. Then the ledger repository
//! from base config's `[assay].ledger_repo`, the three tokens and the verdict App's ids:
//! `ASSAY_GITHUB_TOKEN` (the `assay-verdict` App's: the live PR, the verdict check and the
//! summary comment), `ASSAY_LEDGER_TOKEN` (the PR's ledger state, the per-head publish lock, the
//! decision pointer and the register, through [`assay_app::LedgerPublish`]) and
//! `ASSAY_RUN_TOKEN` (the job's `github.token`: the default branch and the uploaded decision
//! bundle, read back to confirm it). [`run_republish`] does the rest. It writes no output. A
//! variable's value is never shown.

use std::convert::Infallible;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_app::{
    LedgerPublish, LedgerPublishError, ParseLedgerTimeError, PersistOutcome, read_rubric,
};
use assay_cli::outputs::DecideOnlyGuardOutputs;
use assay_cli::{
    FirstAttempt, RepublishJob, RepublishJobPorts, RepublishRun, RepublishRunError, RunVariables,
    run_republish,
};
use assay_domain::Rubric;
use assay_forge::github::{
    Api, BundleFetchError, ForgeError, GhError, GitHub, LedgerRefsError, LedgerRepoError,
    Repository, ThreadSleep,
};

use crate::check::RUBRIC;
use crate::decide_job::{Common, CommonVariables, Tokens, Variable, close, first_attempt, tokens};
use crate::doctor::write_chain;
use crate::guard_job::ACTIONS;
use crate::ledger_job::{
    self, Gh, LedgerJob, LedgerPorts, LedgerTransport, Left, Problem, Programs, SystemClock,
    VariableRefusal, ledger_repo,
};
use crate::publish_job::{upload_outcome, what};
use crate::redecide_job::{PR, guard_outputs};

/// The step, as its refusals name it.
const REPUBLISH: LedgerJob = LedgerJob {
    name: "decide-only publish",
    purpose: "decide-only publish records the decision pointer under the per-head lock",
};

/// The flags, in the order `decide-only publish` takes them.
const CONFIG: &str = "--config";
const STATE: &str = "--state";

/// The environment `assay decide-only publish` reads, gathered by `main` (ARC-11).
pub(crate) struct RepublishVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`PR`].
    pub(crate) pr: Option<OsString>,
    /// [`Variable::Persist`].
    pub(crate) persist: Option<OsString>,
    /// The tokens and ids, as `decide-only decide` reads them.
    pub(crate) common: CommonVariables,
}

/// How `assay decide-only publish` ended: what it published, as one line, or why it did not.
pub(crate) type Ended = ledger_job::Ended<RepublishCliError>;

/// What [`run_republish`] fails with over the live ports.
type RepublishFailure = RepublishRunError<
    ForgeError<GhError>,
    LedgerRepoError<GhError>,
    LedgerPublishError<LedgerRepoError<GhError>, LedgerRefsError<GhError>, ParseLedgerTimeError>,
>;

/// Why `assay decide-only publish` stopped; exit 2.
#[derive(Debug)]
pub(crate) enum RepublishCliError {
    /// Not exactly `--config <plugins-toml> --state <dir>`.
    Usage,
    /// What `decide` refuses alike: the run, a variable, the needs, the rubric, the setup.
    Common(Common),
    /// [`PR`] is missing or not a positive decimal number.
    Pr(Problem),
    /// [`PR`] is not one of guard's `prs`.
    NotScheduled(NonZeroU64),
    /// The verdict could not be published.
    Publish(Box<RepublishFailure>),
}

impl fmt::Display for RepublishCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let job = REPUBLISH.name;
        match self {
            Self::Usage => f.write_str(
                "usage: assay decide-only publish --config <plugins-toml> --state <dir>",
            ),
            Self::Common(common) => common.write(job, f),
            Self::Pr(problem) => VariableRefusal {
                job,
                name: PR,
                holds: "the PR this job publishes for, a positive decimal number",
                problem: *problem,
            }
            .fmt(f),
            Self::NotScheduled(pr) => write!(f, "{job}: {PR} {pr} is not one of guard's prs"),
            Self::Publish(source) => {
                write!(f, "{job}: ")?;
                write_chain(f, source.as_ref())
            }
        }
    }
}

/// The two paths `decide-only publish` takes.
struct Args {
    config: PathBuf,
    state: PathBuf,
}

/// What the publication itself needs, read before any port exists.
struct Job {
    run: FirstAttempt,
    guard: DecideOnlyGuardOutputs,
    pr: NonZeroU64,
    upload: PersistOutcome,
    rubric: Rubric,
    state: PathBuf,
}

/// Runs `assay decide-only publish` with `args` (after `publish`) and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: RepublishVariables) -> Ended {
    let mut left = Vec::new();
    let line = prepare(args, &variables)
        .and_then(|(job, ledger, tokens)| connect(&job, (ledger, tokens), variables, &mut left));
    Ended { line, left }
}

/// `--config <plugins-toml> --state <dir>`, exactly.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, RepublishCliError> {
    let mut flag = |name: &str| match (args.next(), args.next()) {
        (Some(flag), Some(value)) if flag == name => Ok(PathBuf::from(value)),
        _ => Err(RepublishCliError::Usage),
    };
    let parsed = Args {
        config: flag(CONFIG)?,
        state: flag(STATE)?,
    };
    match args.next() {
        None => Ok(parsed),
        Some(_) => Err(RepublishCliError::Usage),
    }
}

/// Everything it can refuse before it starts a process, the arguments and the run first, and
/// every token after everything else.
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &RepublishVariables,
) -> Result<(Job, Repository, Tokens), RepublishCliError> {
    let args = parse(args)?;
    let run = first_attempt(&variables.run).map_err(RepublishCliError::Common)?;
    let guard =
        guard_outputs(variables.common.needs.as_ref()).map_err(RepublishCliError::Common)?;
    let pr = ledger_job::id(variables.pr.as_deref()).map_err(RepublishCliError::Pr)?;
    if !guard.prs().as_slice().contains(&pr) {
        return Err(RepublishCliError::NotScheduled(pr));
    }
    let upload = upload_outcome(variables.persist.as_ref()).map_err(RepublishCliError::Common)?;
    let rubric =
        read_rubric(RUBRIC).map_err(|error| RepublishCliError::Common(Common::Rubric(error)))?;
    let ledger = ledger_repo(REPUBLISH, &args.config)
        .map_err(|error| RepublishCliError::Common(Common::Setup(error)))?;
    let tokens = tokens(&variables.common).map_err(RepublishCliError::Common)?;
    let job = Job {
        run,
        guard,
        pr,
        upload,
        rubric,
        state: args.state,
    };
    Ok((job, ledger, tokens))
}

/// Builds the ledger's transport and the two GitHub adapters, publishes over them, and closes
/// each, pushing onto `left` each directory that could not be removed.
fn connect(
    job: &Job,
    (ledger, tokens): (Repository, Tokens),
    variables: RepublishVariables,
    left: &mut Vec<Left>,
) -> Result<String, RepublishCliError> {
    let programs = Programs {
        path: variables.common.path,
        gh: variables.common.gh,
    };
    let Tokens {
        app,
        ledger: ledger_token,
        run,
        identity,
    } = tokens;
    LedgerTransport::open(REPUBLISH, (ledger, ledger_token), programs, left)
        .map_err(|error| RepublishCliError::Common(Common::Setup(error)))?
        .run(left, |ports, left| {
            let setup = |error| RepublishCliError::Common(Common::Setup(error));
            // Each adapter stores the repository; the run keeps its own for the job.
            let repository = job.run.run_env().repo.clone();
            let app_gh = ports.gh.transport(app).map_err(setup)?;
            let verdict = GitHub::new(Api::new(app_gh, ThreadSleep), repository.clone(), identity);
            let published = ports.gh.transport(run).map_err(setup).and_then(|run_gh| {
                // No read through the run token is attributed to an App.
                let store = GitHub::new(Api::new(run_gh, ThreadSleep), repository, ACTIONS);
                let published = publish_over(job, (&verdict, &store), &ports);
                close(store, Variable::RunToken.name(), left);
                published
            });
            close(verdict, Variable::Token.name(), left);
            published
        })
}

/// The publication through the App's GitHub (`verdict`), the run token's (`store`) and the
/// ledger's files and lock.
fn publish_over(
    job: &Job,
    (verdict, store): (&GitHub<Gh, ThreadSleep>, &GitHub<Gh, ThreadSleep>),
    ledger: &LedgerPorts<'_, '_>,
) -> Result<String, RepublishCliError> {
    let run_env = job.run.run_env();
    let repository = run_env.repo.to_string();
    let lock = LedgerPublish {
        ledger: ledger.files,
        refs: ledger.refs,
        clock: &SystemClock,
        run_uid: run_env.run_uid,
        repo: &repository,
    };
    let republish_job = RepublishJob {
        run: &job.run,
        guard: &job.guard,
        pr: job.pr,
        rubric: &job.rubric,
        upload: job.upload,
    };
    let ports = RepublishJobPorts {
        app: verdict,
        run: store,
        ledger: ledger.files,
        lock: &lock,
    };
    run_republish(&republish_job, &ports, &job.state)
        .map(|published| line(&published))
        .map_err(|error| RepublishCliError::Publish(Box::new(error)))
}

/// What it published, as one line for the job's log.
fn line<L: Error + 'static>(published: &RepublishRun<L, BundleFetchError>) -> String {
    match published {
        RepublishRun::Silent => {
            "assay decide-only publish: decide left nothing to publish".to_owned()
        }
        RepublishRun::Unclaimed => "assay decide-only publish: nothing is published: the \
                                    register no longer names the PR's live head from this \
                                    evidence (another run published, or the PR moved)"
            .to_owned(),
        RepublishRun::Refused(published) => format!(
            "assay decide-only publish: the refusal {}",
            what::<L, Infallible>(published)
        ),
        RepublishRun::Decided(published) => {
            format!(
                "assay decide-only publish: the re-decision {}",
                what(published)
            )
        }
    }
}
