//! `assay decide-only decide --config <plugins-toml> --out <dir>` (#61 DO-11, D9 A): the
//! redecide job's step, one job per PR of guard's `prs`.
//!
//! In order, everything it can refuse before a token is read: its flags; the run, refused for
//! any attempt but the first (J2, R3-05); the regular file `GITHUB_OUTPUT` names (opened, never
//! created); guard's outputs from `ASSAY_NEEDS` (`guard` alone); `ASSAY_PR`, which must be one
//! of guard's `prs`; the event from `GITHUB_EVENT_NAME` and the payload `GITHUB_EVENT_PATH`
//! names, the only place a command is read from (J1); `--out` as an [`OutDir`]; rubric v1. Then
//! the ledger repository from base config's `[assay].ledger_repo`, the three tokens and the
//! verdict App's ids: `ASSAY_LEDGER_TOKEN` (the ledger's files), `ASSAY_GITHUB_TOKEN` (the
//! `assay-verdict` App's: the live PR, the base tip's lists, reviews, the command comments and
//! replies) and `ASSAY_RUN_TOKEN` (the job's `github.token`: the evidence run's provenance, its
//! bundle and the default branch). [`run_redecide`] writes the state and, for a new decision,
//! the bundle into `--out`; the outputs (03 §4a) are appended after. Nothing is published
//! here. A variable's value is never shown.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::SystemTime;

use assay_app::{DECISION_FILE, LedgerTime, NonceSource, ParseLedgerTimeError, read_rubric};
use assay_cli::outputs::{DecideOnlyGuardOutputs, RedecideOutputs};
use assay_cli::{
    FirstAttempt, JobName, Needs, OutDir, OutDirError, OutputError, Outputs, RedecideJob,
    RedecideJobPorts, RedecideRunError, RunVariables, STATE_FILE, run_redecide,
};
use assay_domain::Rubric;
use assay_forge::github::{
    Api, BundleFetchError, DecideOnlyEvent, DecideOnlyEventError, DecideOnlyEventName, ForgeError,
    GhError, GitHub, LedgerRepoError, Repository, ThreadSleep, TipListsError,
};

use crate::check::RUBRIC;
use crate::decide_job::{
    Common, CommonVariables, Tokens, Variable, close, first_attempt, present, tokens,
};
use crate::doctor::{Urandom, write_chain};
use crate::guard_job::{ACTIONS, EVENT_NAME, EVENT_PATH, open_output, open_payload};
use crate::ledger_job::{
    self, Gh, LedgerJob, LedgerPorts, LedgerTransport, Left, Problem, Programs, VariableRefusal,
    ledger_repo,
};

/// The job, as its refusals name it.
const REDECIDE: LedgerJob = LedgerJob {
    name: "decide-only decide",
    purpose: "decide-only decide counts commands and reads the PR's ledger state",
};

/// The matrix entry: the PR this job decides.
pub(crate) const PR: &str = "ASSAY_PR";

/// The flags, in the order `decide-only decide` takes them.
const CONFIG: &str = "--config";
const OUT: &str = "--out";

/// The environment `assay decide-only decide` reads, gathered by `main` (ARC-11).
pub(crate) struct RedecideVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`Variable::Output`].
    pub(crate) output: Option<OsString>,
    /// [`PR`].
    pub(crate) pr: Option<OsString>,
    /// [`EVENT_NAME`].
    pub(crate) event_name: Option<OsString>,
    /// [`EVENT_PATH`].
    pub(crate) event_path: Option<OsString>,
    /// The tokens and ids, as `decide` reads them.
    pub(crate) common: CommonVariables,
    /// Now: the command record's time and every event's.
    pub(crate) now: SystemTime,
}

/// How `assay decide-only decide` ended: what it wrote, as one line, or why it did not.
pub(crate) type Ended = ledger_job::Ended<RedecideCliError>;

/// What [`run_redecide`] fails with over the live ports.
type RedecideFailure = RedecideRunError<
    ForgeError<GhError>,
    TipListsError<GhError>,
    BundleFetchError,
    LedgerRepoError<GhError>,
>;

/// Why `assay decide-only decide` stopped; exit 2.
#[derive(Debug)]
pub(crate) enum RedecideCliError {
    /// Not exactly `--config <plugins-toml> --out <dir>`.
    Usage,
    /// What `decide` refuses alike: the run, a variable, the needs, the rubric, the setup.
    Common(Common),
    /// `GITHUB_OUTPUT` could not be opened for appending, or is not a regular file.
    OutputFile(io::Error),
    /// [`PR`] is missing or not a positive decimal number.
    Pr(Problem),
    /// [`PR`] is not one of guard's `prs`.
    NotScheduled(NonZeroU64),
    /// [`EVENT_NAME`] or [`EVENT_PATH`] is not set.
    EventVariable(&'static str),
    /// [`EVENT_NAME`] names an event decide-only does not run for.
    Event,
    /// The payload file could not be opened, or is not a regular file.
    PayloadFile(io::Error),
    /// The payload was refused.
    Payload(DecideOnlyEventError),
    /// `--out` was refused.
    Out(OutDirError),
    /// No artifact nonce could be read.
    Nonce(io::Error),
    /// The system clock is not a ledger time.
    Clock(ParseLedgerTimeError),
    /// The PR could not be re-decided, or its state written.
    Redecide(Box<RedecideFailure>),
    /// An output could not be appended.
    Output(OutputError),
}

impl fmt::Display for RedecideCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let job = REDECIDE.name;
        let (context, source): (&str, &dyn Error) = match self {
            Self::Usage => {
                return f.write_str(
                    "usage: assay decide-only decide --config <plugins-toml> --out <dir>",
                );
            }
            Self::Common(common) => return common.write(job, f),
            Self::OutputFile(source) => {
                return write!(f, "{job}: opening {}: {source}", Variable::Output.name());
            }
            Self::Pr(problem) => {
                let refusal = VariableRefusal {
                    job,
                    name: PR,
                    holds: "the PR this job decides, a positive decimal number",
                    problem: *problem,
                };
                return refusal.fmt(f);
            }
            Self::NotScheduled(pr) => {
                return write!(f, "{job}: {PR} {pr} is not one of guard's prs");
            }
            Self::EventVariable(name) => return write!(f, "{job}: {name} is not set"),
            Self::Event => {
                return write!(
                    f,
                    "{job}: {EVENT_NAME}: decide-only runs only for issue_comment, \
                     pull_request_target and schedule"
                );
            }
            Self::PayloadFile(source) => {
                return write!(f, "{job}: reading {EVENT_PATH}: {source}");
            }
            Self::Payload(source) => ("", source),
            Self::Out(source) => ("", source),
            Self::Nonce(source) => ("the artifact nonce: ", source),
            Self::Clock(source) => ("the system clock: ", source),
            Self::Redecide(source) => ("", source.as_ref()),
            Self::Output(source) => ("", source),
        };
        write!(f, "{job}: {context}")?;
        write_chain(f, source)
    }
}

/// The two paths `decide-only decide` takes.
struct Args {
    config: PathBuf,
    out: PathBuf,
}

/// What the re-decision itself needs, read before any port exists.
struct Job {
    run: FirstAttempt,
    output: File,
    guard: DecideOnlyGuardOutputs,
    pr: NonZeroU64,
    event: DecideOnlyEvent,
    out: OutDir,
    rubric: Rubric,
}

/// Runs `assay decide-only decide` with `args` (after `decide`) and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: RedecideVariables) -> Ended {
    let mut left = Vec::new();
    let line = prepare(args, &variables)
        .and_then(|(job, ledger, tokens)| connect(&job, (ledger, tokens), variables, &mut left));
    Ended { line, left }
}

/// `--config <plugins-toml> --out <dir>`, exactly.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, RedecideCliError> {
    let mut flag = |name: &str| match (args.next(), args.next()) {
        (Some(flag), Some(value)) if flag == name => Ok(PathBuf::from(value)),
        _ => Err(RedecideCliError::Usage),
    };
    let parsed = Args {
        config: flag(CONFIG)?,
        out: flag(OUT)?,
    };
    match args.next() {
        None => Ok(parsed),
        Some(_) => Err(RedecideCliError::Usage),
    }
}

/// Everything it can refuse before it starts a process, the arguments and the run first, and
/// every token after everything else.
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &RedecideVariables,
) -> Result<(Job, Repository, Tokens), RedecideCliError> {
    let args = parse(args)?;
    let run = first_attempt(&variables.run).map_err(RedecideCliError::Common)?;
    let output =
        present(variables.output.as_ref(), Variable::Output).map_err(RedecideCliError::Common)?;
    let output = open_output(output).map_err(RedecideCliError::OutputFile)?;
    let guard = guard_outputs(variables.common.needs.as_ref()).map_err(RedecideCliError::Common)?;
    let pr = ledger_job::id(variables.pr.as_deref()).map_err(RedecideCliError::Pr)?;
    if !guard.prs().as_slice().contains(&pr) {
        return Err(RedecideCliError::NotScheduled(pr));
    }
    let event = event(variables)?;
    let out = OutDir::new(&args.out).map_err(RedecideCliError::Out)?;
    let rubric =
        read_rubric(RUBRIC).map_err(|error| RedecideCliError::Common(Common::Rubric(error)))?;
    let ledger = ledger_repo(REDECIDE, &args.config)
        .map_err(|error| RedecideCliError::Common(Common::Setup(error)))?;
    let tokens = tokens(&variables.common).map_err(RedecideCliError::Common)?;
    let job = Job {
        run,
        output,
        guard,
        pr,
        event,
        out,
        rubric,
    };
    Ok((job, ledger, tokens))
}

/// guard's outputs, from `ASSAY_NEEDS` holding `guard` alone.
fn guard_outputs(value: Option<&OsString>) -> Result<DecideOnlyGuardOutputs, Common> {
    let needs = present(value, Variable::Needs)?;
    let needs = Needs::parse(needs.as_encoded_bytes(), &[JobName::Guard]).map_err(Common::Needs)?;
    DecideOnlyGuardOutputs::from_needs(&needs).map_err(Common::Needs)
}

/// The triggering event's payload, as guard read it.
fn event(variables: &RedecideVariables) -> Result<DecideOnlyEvent, RedecideCliError> {
    let name = variables
        .event_name
        .as_ref()
        .ok_or(RedecideCliError::EventVariable(EVENT_NAME))?
        .to_str()
        .and_then(|name| name.parse::<DecideOnlyEventName>().ok())
        .ok_or(RedecideCliError::Event)?;
    let path = variables
        .event_path
        .as_ref()
        .ok_or(RedecideCliError::EventVariable(EVENT_PATH))?;
    let file = open_payload(path).map_err(RedecideCliError::PayloadFile)?;
    DecideOnlyEvent::read(name, file).map_err(RedecideCliError::Payload)
}

/// Builds the ledger's transport and the two GitHub adapters, re-decides over them, and closes
/// each, pushing onto `left` each directory that could not be removed.
fn connect(
    job: &Job,
    (ledger, tokens): (Repository, Tokens),
    variables: RedecideVariables,
    left: &mut Vec<Left>,
) -> Result<String, RedecideCliError> {
    let at = LedgerTime::from_system_time(variables.now).map_err(RedecideCliError::Clock)?;
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
    LedgerTransport::open(REDECIDE, (ledger, ledger_token), programs, left)
        .map_err(|error| RedecideCliError::Common(Common::Setup(error)))?
        .run(left, |ports, left| {
            let setup = |error| RedecideCliError::Common(Common::Setup(error));
            // Each adapter stores the repository; the run keeps its own for the job.
            let repository = job.run.run_env().repo.clone();
            let app_gh = ports.gh.transport(app).map_err(setup)?;
            let verdict = GitHub::new(Api::new(app_gh, ThreadSleep), repository.clone(), identity);
            let decided = ports.gh.transport(run).map_err(setup).and_then(|run_gh| {
                // No read through the run token is attributed to an App.
                let github = GitHub::new(Api::new(run_gh, ThreadSleep), repository, ACTIONS);
                let decided = redecide_over(job, at, (&verdict, &github), &ports);
                close(github, Variable::RunToken.name(), left);
                decided
            });
            close(verdict, Variable::Token.name(), left);
            let outputs = decided?;
            let mut writer = Outputs::new(&job.output);
            for (name, value) in outputs.outputs() {
                writer
                    .write(name, &value)
                    .map_err(RedecideCliError::Output)?;
            }
            Ok(line(&job.out, &outputs))
        })
}

/// The re-decision over the App's GitHub (`verdict`), the run token's (`github`) and the
/// ledger's files.
fn redecide_over(
    job: &Job,
    at: LedgerTime,
    (verdict, github): (&GitHub<Gh, ThreadSleep>, &GitHub<Gh, ThreadSleep>),
    ledger: &LedgerPorts<'_, '_>,
) -> Result<RedecideOutputs, RedecideCliError> {
    let nonce = Urandom.nonce().map_err(RedecideCliError::Nonce)?;
    let redecide_job = RedecideJob {
        run: &job.run,
        guard: &job.guard,
        pr: job.pr,
        event: &job.event,
        rubric: &job.rubric,
        at,
        nonce,
    };
    let ports = RedecideJobPorts {
        app: verdict,
        run: github,
        ledger: ledger.files,
    };
    run_redecide(&redecide_job, &ports, &job.out)
        .map_err(|error| RedecideCliError::Redecide(Box::new(error)))
}

/// What it wrote, as one line for the job's log.
fn line(out: &OutDir, outputs: &RedecideOutputs) -> String {
    let state = out.path().join(STATE_FILE);
    match outputs {
        RedecideOutputs::NoBundle => {
            format!(
                "assay decide-only decide: wrote {}; no bundle",
                state.display()
            )
        }
        RedecideOutputs::Bundle { digest, artifact } => format!(
            "assay decide-only decide: wrote {} and {} ({digest}) for {artifact}",
            state.display(),
            out.path().join(DECISION_FILE).display(),
        ),
    }
}
