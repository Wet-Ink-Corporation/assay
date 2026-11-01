//! `assay publish --config <plugins-toml> --state <dir>` (#60 ST-14, Z4, D5 A): the decide
//! job's last step, after `upload-artifact`.
//!
//! In order, everything it can refuse before a token is read: its flags; the run, refused for
//! any attempt but the first (R3-05, I1); every upstream job's outputs from `ASSAY_NEEDS`, read
//! as decide read them (I2, I6); the upload step's outcome from `ASSAY_PERSIST`, exactly one of
//! GitHub's four words (I6); rubric v1. Then the ledger repository from base config's
//! `[assay].ledger_repo` (D3 A), the three tokens and the verdict App's ids:
//! `ASSAY_GITHUB_TOKEN` (the `assay-verdict` App's: the verdict check and the summary comment),
//! `ASSAY_LEDGER_TOKEN` (the per-head publish lock and the evidence pointer, through
//! [`assay_app::LedgerPublish`] over the ledger's files and refs) and `ASSAY_RUN_TOKEN` (the
//! job's `github.token`: the uploaded bundle, read back to confirm it). [`run_publish`] does
//! the rest. A variable's value is never shown.

use std::convert::Infallible;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use assay_app::{LedgerPublish, PersistFailure, PersistOutcome, Published, Unlock, read_rubric};
use assay_cli::outputs::DecideNeeds;
use assay_cli::{FirstAttempt, PublishJob, PublishJobPorts, PublishRun, RunVariables, run_publish};
use assay_domain::Rubric;
use assay_forge::github::{Api, BundleFetchError, GitHub, Repository, ThreadSleep};

use crate::check::RUBRIC;
use crate::decide_job::{
    Common, CommonVariables, PublishCliError, Tokens, Variable, close, decide_needs, first_attempt,
    present, tokens,
};
use crate::doctor::write_chain;
use crate::guard_job::ACTIONS;
use crate::ledger_job::{
    self, LedgerJob, LedgerPorts, LedgerTransport, Left, Problem, Programs, SystemClock,
    ledger_repo,
};

/// The publish job, as its refusals name it.
const PUBLISH: LedgerJob = LedgerJob {
    name: "publish",
    purpose: "publish records the evidence pointer under the per-head lock",
};

/// The flags, in the order `publish` takes them.
const CONFIG: &str = "--config";
const STATE: &str = "--state";

/// The environment `assay publish` reads, gathered by `main` (ARC-11).
pub(crate) struct PublishVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`Variable::Persist`].
    pub(crate) persist: Option<OsString>,
    /// The tokens and ids both commands read.
    pub(crate) common: CommonVariables,
}

/// How `assay publish` ended: what it published, as one line, or why it did not.
pub(crate) type Ended = ledger_job::Ended<PublishCliError>;

/// The two paths `publish` takes.
struct Args {
    config: PathBuf,
    state: PathBuf,
}

/// What the publication itself needs, read before any port exists.
struct Job {
    run: FirstAttempt,
    needs: DecideNeeds,
    upload: PersistOutcome,
    rubric: Rubric,
    state: PathBuf,
}

/// Everything publish reads before it starts a process.
struct Prepared {
    job: Job,
    ledger: Repository,
    tokens: Tokens,
}

/// Runs `assay publish` with `args` (after `publish`) and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: PublishVariables) -> Ended {
    let mut left = Vec::new();
    let line =
        prepare(args, &variables).and_then(|prepared| connect(prepared, variables, &mut left));
    Ended { line, left }
}

/// `--config <plugins-toml> --state <dir>`, exactly.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, PublishCliError> {
    let mut flag = |name: &str| match (args.next(), args.next()) {
        (Some(flag), Some(value)) if flag == name => Ok(PathBuf::from(value)),
        _ => Err(PublishCliError::Usage),
    };
    let parsed = Args {
        config: flag(CONFIG)?,
        state: flag(STATE)?,
    };
    match args.next() {
        None => Ok(parsed),
        Some(_) => Err(PublishCliError::Usage),
    }
}

/// How the upload step ended, from [`Variable::Persist`]: exactly one of GitHub's four words.
pub(crate) fn upload_outcome(value: Option<&OsString>) -> Result<PersistOutcome, Common> {
    present(value, Variable::Persist).and_then(|value| {
        value
            .to_str()
            .and_then(|text| text.parse().ok())
            .ok_or(Common::Variable {
                variable: Variable::Persist,
                problem: Problem::Invalid,
            })
    })
}

/// Everything publish can refuse before it starts a process, the arguments and the run first.
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &PublishVariables,
) -> Result<Prepared, PublishCliError> {
    let args = parse(args)?;
    let run = first_attempt(&variables.run).map_err(PublishCliError::Common)?;
    let needs = decide_needs(variables.common.needs.as_ref()).map_err(PublishCliError::Common)?;
    let upload = upload_outcome(variables.persist.as_ref()).map_err(PublishCliError::Common)?;
    let rubric =
        read_rubric(RUBRIC).map_err(|error| PublishCliError::Common(Common::Rubric(error)))?;
    let ledger = ledger_repo(PUBLISH, &args.config)
        .map_err(|error| PublishCliError::Common(Common::Setup(error)))?;
    let tokens = tokens(&variables.common).map_err(PublishCliError::Common)?;
    Ok(Prepared {
        job: Job {
            run,
            needs,
            upload,
            rubric,
            state: args.state,
        },
        ledger,
        tokens,
    })
}

/// Builds the ledger's transport, publishes over it, and removes the directories, pushing onto
/// `left` each one that could not be removed.
fn connect(
    prepared: Prepared,
    variables: PublishVariables,
    left: &mut Vec<Left>,
) -> Result<String, PublishCliError> {
    let Prepared {
        job,
        ledger,
        tokens,
    } = prepared;
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
    LedgerTransport::open(PUBLISH, (ledger, ledger_token), programs, left)
        .map_err(|error| PublishCliError::Common(Common::Setup(error)))?
        .run(left, |ports, left| {
            let setup = |error| PublishCliError::Common(Common::Setup(error));
            // Each adapter stores the repository; the run keeps its own for the job.
            let repository = job.run.run_env().repo.clone();
            let app_gh = ports.gh.transport(app).map_err(setup)?;
            let verdict = GitHub::new(Api::new(app_gh, ThreadSleep), repository.clone(), identity);
            let published = ports.gh.transport(run).map_err(setup).and_then(|run_gh| {
                // No read through the run token is attributed to an App.
                let store = GitHub::new(Api::new(run_gh, ThreadSleep), repository, ACTIONS);
                let published = publish_over(&job, (&verdict, &store), &ports);
                close(store, Variable::RunToken.name(), left);
                published
            });
            close(verdict, Variable::Token.name(), left);
            published
        })
}

/// The App's GitHub (`verdict`) and the run token's (`store`).
type Live<'a> = (
    &'a GitHub<ledger_job::Gh, ThreadSleep>,
    &'a GitHub<ledger_job::Gh, ThreadSleep>,
);

/// The publication through the verdict App's check, the ledger's lock and the artifact store.
fn publish_over(
    job: &Job,
    (verdict, store): Live<'_>,
    ledger: &LedgerPorts<'_, '_>,
) -> Result<String, PublishCliError> {
    let run_env = job.run.run_env();
    let repository = run_env.repo.to_string();
    let lock = LedgerPublish {
        ledger: ledger.files,
        refs: ledger.refs,
        clock: &SystemClock,
        run_uid: run_env.run_uid,
        repo: &repository,
    };
    let publish_job = PublishJob {
        run: &job.run,
        identity: &job.needs.identity,
        cost: job.needs.review.cost,
        rubric: &job.rubric,
        upload: job.upload,
    };
    let ports = PublishJobPorts {
        forge: verdict,
        ledger: &lock,
        bundles: store,
    };
    run_publish(&publish_job, &ports, &job.state)
        .map(|published| line(&published))
        .map_err(|error| PublishCliError::Publish(Box::new(error)))
}

/// What publish did, as one line for the job's log.
fn line<L: Error + 'static>(published: &PublishRun<L, BundleFetchError>) -> String {
    match published {
        PublishRun::Silent => "assay publish: superseded, so nothing is published".to_owned(),
        PublishRun::Refused(published) => {
            format!(
                "assay publish: the refusal {}",
                what::<L, Infallible>(published)
            )
        }
        PublishRun::Decided(published) => {
            format!("assay publish: the decision {}", what(published))
        }
    }
}

/// What became of a publication, as a clause.
pub(crate) fn what<L: Error + 'static, B: Error + 'static>(published: &Published<L, B>) -> String {
    let unlock = |unlock: &Unlock<L>| match unlock {
        Unlock::NotTaken | Unlock::Released => String::new(),
        Unlock::Failed(error) => {
            format!("; the publish lock was not released and expires with its lease: {error}")
        }
    };
    match published {
        Published::Nothing => "is not published".to_owned(),
        Published::Unchanged => "is already on the check".to_owned(),
        Published::Yielded { unlock: lock, .. } => {
            format!(
                "is not published: the check is a newer run's{}",
                unlock(lock)
            )
        }
        Published::Written(written) => {
            let persist = written
                .persist
                .as_ref()
                .map_or_else(String::new, |failure| {
                    format!("; not persisted ({}): {}", failure.code(), Persist(failure))
                });
            format!("is published{persist}{}", unlock(&written.unlock))
        }
    }
}

/// A persist failure's cause chain, for the log.
struct Persist<'a, L, B>(&'a PersistFailure<L, B>);

impl<L, B> fmt::Display for Persist<'_, L, B>
where
    L: Error + 'static,
    B: Error + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            PersistFailure::Bundle(error) => write_chain(f, error),
            PersistFailure::Upload(error) => write_chain(f, error),
            PersistFailure::Ledger(error) => write_chain(f, error),
        }
    }
}
