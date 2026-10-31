//! `assay admit --config <plugins-toml> --artifacts <dir> --out <dir>` (#60 ST-11, S1a, Z4):
//! the pipeline's admit job.
//!
//! In order, everything it can refuse before a token is read: its flags; the run, refused for
//! any attempt but the first (R3-05, I1); the regular file `GITHUB_OUTPUT` names (opened, never
//! created); guard's identity and plan's outputs from `ASSAY_NEEDS` (I2); `--out` as an
//! [`OutDir`]; plan's `run.json` in `<artifacts>/<artifact_plan>/` (a regular file, no symlink,
//! at most [`MAX_DOCUMENT_BYTES`]); the contract schemas. Then the ledger repository from base
//! config's `[assay].ledger_repo` (D3 A) and the three tokens: `ASSAY_LEDGER_TOKEN` (the
//! ledger's files and refs, the slot's deletion pushed from an object-only repository in a
//! scratch directory, [`crate::ledger_job`]), `ASSAY_GITHUB_TOKEN` (the `assay-verdict` App's,
//! for the command records' comments) and `ASSAY_RUN_TOKEN` (the job's `github.token`: the
//! pinned commits fetched into a second object-only repository, the range's commit identities
//! and the previous run's bundle). [`run_admit`] does the rest; a run with no LLM stage calls
//! no port. admit's outputs (03 §4a) are appended after its documents; when the documents
//! cannot be written once a slot is held, `slot` and `lease_commit` alone are appended so
//! release still releases it. A variable's value is never shown.

mod error;

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use assay_app::{ArtifactJob, Clock, CommitId, ContractValidator, MAX_DOCUMENT_BYTES};
use assay_cli::outputs::{AdmitOutputs, Identity, PlanOutputs};
use assay_cli::{
    AdmitJob, AdmitJobPorts, AdmitRunError, FetchCommits, FirstAttempt, ForgePullRequest, JobName,
    Needs, OutDir, OutputName, Outputs, RUN_FILE, RunEnv, RunEnvError, RunVariables, run_admit,
};
use assay_forge::git::{
    Authorization, CommitSha, GitEnvironment, GitLimits, HttpsRemote, ObjectRepo, TreeLimits,
};
use assay_forge::github::{Api, GhEnvironment, GhToken, GitHub, Repository, ThreadSleep};
use assay_forge::process::GroupKiller;

pub(crate) use self::error::{AdmitCliError, FetchError, Variable};
use crate::check::DIFF_CAP;
use crate::guard_job::{ACTIONS, open_output};
use crate::ledger_job::{
    self, Gh, LedgerJob, LedgerPorts, LedgerTransport, Left, Programs, ledger_repo,
};

/// The admit job, as its refusals name it.
const ADMIT: LedgerJob = LedgerJob {
    name: "admit",
    purpose: "admit reserves the run's review in the ledger",
};

/// The flags, in the order `admit` takes them.
const CONFIG: &str = "--config";
const ARTIFACTS: &str = "--artifacts";
const OUT: &str = "--out";

/// The jobs `admit` needs (04 §1 S1a).
const NEEDED: [JobName; 2] = [JobName::Guard, JobName::Plan];

/// The environment `assay admit` reads, gathered by `main` (ARC-11).
pub(crate) struct AdmitVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`Variable::Output`].
    pub(crate) output: Option<OsString>,
    /// [`Variable::Needs`].
    pub(crate) needs: Option<OsString>,
    /// [`Variable::Token`].
    pub(crate) token: Option<OsString>,
    /// [`Variable::LedgerToken`].
    pub(crate) ledger_token: Option<OsString>,
    /// [`Variable::RunToken`].
    pub(crate) run_token: Option<OsString>,
    /// `PATH`, which finds `gh` and git.
    pub(crate) path: Option<OsString>,
    /// What `gh` inherits.
    pub(crate) gh: GhEnvironment,
}

/// How `assay admit` ended: what admit decided, as one line, or why it did not.
pub(crate) type Ended = ledger_job::Ended<AdmitCliError>;

/// The three paths `admit` takes.
struct Args {
    config: PathBuf,
    artifacts: PathBuf,
    out: PathBuf,
}

/// What the admission itself needs, read before any port exists.
struct Job {
    run: FirstAttempt,
    output: File,
    identity: Identity,
    plan: PlanOutputs,
    out: OutDir,
    document: Vec<u8>,
    validator: ContractValidator,
}

/// The tokens, each for its own transport.
struct Tokens {
    app: GhToken,
    ledger: GhToken,
    run: GhToken,
}

/// Everything admit reads before it starts a process.
struct Prepared {
    job: Job,
    ledger: Repository,
    tokens: Tokens,
    /// The run token as git's header, and where git sends it.
    fetch: (HttpsRemote, Authorization),
}

/// Runs `assay admit` with `args` (after `admit`) and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: AdmitVariables) -> Ended {
    let mut left = Vec::new();
    let line =
        prepare(args, &variables).and_then(|prepared| connect(prepared, variables, &mut left));
    Ended { line, left }
}

/// `--config <plugins-toml> --artifacts <dir> --out <dir>`, exactly.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, AdmitCliError> {
    let mut flag = |name: &str| match (args.next(), args.next()) {
        (Some(flag), Some(value)) if flag == name => Ok(PathBuf::from(value)),
        _ => Err(AdmitCliError::Usage),
    };
    let parsed = Args {
        config: flag(CONFIG)?,
        artifacts: flag(ARTIFACTS)?,
        out: flag(OUT)?,
    };
    match args.next() {
        None => Ok(parsed),
        Some(_) => Err(AdmitCliError::Usage),
    }
}

/// Everything admit can refuse before it starts a process, the arguments and the run first.
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &AdmitVariables,
) -> Result<Prepared, AdmitCliError> {
    let args = parse(args)?;
    let run = RunEnv::from_vars(&variables.run)
        .and_then(|run| run.first_attempt().map_err(RunEnvError::from))
        .map_err(AdmitCliError::Run)?;
    let output = present(variables.output.as_ref(), Variable::Output)?;
    let output = open_output(output).map_err(AdmitCliError::OutputFile)?;
    let needs = present(variables.needs.as_ref(), Variable::Needs)?;
    let needs = Needs::parse(needs.as_encoded_bytes(), &NEEDED).map_err(AdmitCliError::Needs)?;
    let identity = Identity::from_needs(&needs).map_err(AdmitCliError::Needs)?;
    let plan = PlanOutputs::from_needs(&needs).map_err(AdmitCliError::Needs)?;
    let out = OutDir::new(&args.out).map_err(AdmitCliError::Out)?;
    let document = read_plan(&args.artifacts, &plan)?;
    let validator = ContractValidator::new().map_err(AdmitCliError::Validator)?;
    let ledger = ledger_repo(ADMIT, &args.config).map_err(AdmitCliError::Setup)?;
    let token = |variable: Variable, value: Option<&OsString>| {
        ledger_job::token(value.map(OsString::as_os_str))
            .map_err(|problem| AdmitCliError::Variable { variable, problem })
    };
    let tokens = Tokens {
        app: token(Variable::Token, variables.token.as_ref())?,
        ledger: token(Variable::LedgerToken, variables.ledger_token.as_ref())?,
        run: token(Variable::RunToken, variables.run_token.as_ref())?,
    };
    // git authenticates with the run token, sent to the repository's URL only.
    let authorization = tokens
        .run
        .git_authorization()
        .map_err(AdmitCliError::Authorization)?;
    let remote = format!("https://github.com/{}.git", run.run_env().repo)
        .parse()
        .map_err(AdmitCliError::Remote)?;
    Ok(Prepared {
        job: Job {
            run,
            output,
            identity,
            plan,
            out,
            document,
            validator,
        },
        ledger,
        tokens,
        fetch: (remote, authorization),
    })
}

/// `value`, which must be set.
fn present(value: Option<&OsString>, variable: Variable) -> Result<&OsString, AdmitCliError> {
    value.ok_or(AdmitCliError::Variable {
        variable,
        problem: ledger_job::Problem::Missing,
    })
}

/// The `run.json` of plan's artifact, `<artifacts>/<artifact_plan>/run.json`: a regular file
/// (not a symlink) of at most [`MAX_DOCUMENT_BYTES`], read as it is.
fn read_plan(artifacts: &Path, plan: &PlanOutputs) -> Result<Vec<u8>, AdmitCliError> {
    let name = plan
        .artifacts()
        .get(&ArtifactJob::Plan)
        .ok_or(AdmitCliError::NoPlanArtifact)?;
    let path = artifacts.join(name).join(RUN_FILE);
    let refused = |source| AdmitCliError::PlanArtifact {
        path: path.clone(),
        source,
    };
    let not_regular = || std::io::Error::other("not a regular file");
    if !fs::symlink_metadata(&path).map_err(refused)?.is_file() {
        return Err(refused(not_regular()));
    }
    let file = File::open(&path).map_err(refused)?;
    if !file.metadata().map_err(refused)?.is_file() {
        return Err(refused(not_regular()));
    }
    let cap = u64::try_from(MAX_DOCUMENT_BYTES).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(refused)?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(AdmitCliError::PlanArtifactTooLarge { path });
    }
    Ok(bytes)
}

/// Builds the ledger's transport, admits over it, and removes the directories, pushing onto
/// `left` each one that could not be removed.
fn connect(
    prepared: Prepared,
    variables: AdmitVariables,
    left: &mut Vec<Left>,
) -> Result<String, AdmitCliError> {
    let Prepared {
        job,
        ledger,
        tokens,
        fetch,
    } = prepared;
    // git's environment keeps its own copy of PATH; the ledger's transport takes the other.
    let git_path = variables.path.clone().ok_or(AdmitCliError::NoGit)?;
    let programs = Programs {
        path: variables.path,
        gh: variables.gh,
    };
    let Tokens {
        app,
        ledger: ledger_token,
        run,
    } = tokens;
    LedgerTransport::open(ADMIT, (ledger, ledger_token), programs, left)
        .map_err(AdmitCliError::Setup)?
        .run(left, |ports, left| {
            let repos = Repos { git_path, fetch };
            let live = Live {
                repos: &repos,
                ledger: &ports,
            };
            admit_over(job, (app, run), live, left)
        })
}

/// What the object repository is built and fetched with.
struct Repos {
    git_path: OsString,
    fetch: (HttpsRemote, Authorization),
}

/// What every step below [`connect`] borrows: the object repository's setup and the ledger.
#[derive(Clone, Copy)]
struct Live<'a, 'p, 'r> {
    repos: &'a Repos,
    ledger: &'a LedgerPorts<'p, 'r>,
}

/// The two GitHub adapters (the App's, `app`, for comments; the run token's, `run`, for commit
/// identities and bundles), the object repository in a scratch directory, then the admission;
/// each transport and directory is closed after, and pushed onto `left` when it cannot be.
fn admit_over(
    job: Job,
    (app, run): (GhToken, GhToken),
    live: Live<'_, '_, '_>,
    left: &mut Vec<Left>,
) -> Result<String, AdmitCliError> {
    let ledger = live.ledger;
    // Each adapter stores the repository; the run keeps its own for the job. No read admit
    // makes is attributed to an App, so both carry guard's identity, which none consults.
    let repository = job.run.run_env().repo.clone();
    let app_gh = ledger.gh.transport(app).map_err(AdmitCliError::Setup)?;
    let comments = GitHub::new(Api::new(app_gh, ThreadSleep), repository.clone(), ACTIONS);
    let admitted = ledger
        .gh
        .transport(run)
        .map_err(AdmitCliError::Setup)
        .and_then(|run_gh| {
            let github = GitHub::new(Api::new(run_gh, ThreadSleep), repository, ACTIONS);
            let admitted = in_scratch(job, live, (&github, &comments), left);
            close(github, Variable::RunToken.name(), left);
            admitted
        });
    close(comments, Variable::Token.name(), left);
    admitted
}

/// Closes `github`'s transport, pushing its directory onto `left` when it cannot be removed.
fn close(github: GitHub<Gh, ThreadSleep>, token: &'static str, left: &mut Vec<Left>) {
    if let Err(error) = github.into_api().into_transport().close() {
        left.push(Left::Gh(token, error));
    }
}

/// A scratch directory for the object repository, the admission in it, and the directory
/// removed after, pushed onto `left` when it cannot be.
fn in_scratch(
    job: Job,
    live: Live<'_, '_, '_>,
    github: (&GitHub<Gh, ThreadSleep>, &GitHub<Gh, ThreadSleep>),
    left: &mut Vec<Left>,
) -> Result<String, AdmitCliError> {
    let killer = GroupKiller::system().map_err(AdmitCliError::Killer)?;
    let scratch = tempfile::tempdir().map_err(AdmitCliError::Scratch)?;
    // The environment stores its own PATH.
    let git = GitEnvironment::new(live.repos.git_path.clone(), killer);
    let admitted = ObjectRepo::init(
        scratch.path().join("objects.git"),
        git,
        GitLimits::default(),
    )
    .map_err(AdmitCliError::Git)
    .and_then(|repo| admit_in(job, live, github, &repo));
    if let Err(error) = scratch.close() {
        left.push(Left::Scratch(ADMIT.name, error));
    }
    admitted
}

/// The admission over `repo`, then the outputs.
fn admit_in(
    job: Job,
    live: Live<'_, '_, '_>,
    (github, comments): (&GitHub<Gh, ThreadSleep>, &GitHub<Gh, ThreadSleep>),
    repo: &ObjectRepo,
) -> Result<String, AdmitCliError> {
    let (ledger, (remote, authorization)) = (live.ledger, &live.repos.fetch);
    let fetch = Fetcher {
        repo,
        remote,
        authorization,
    };
    let objects = ForgePullRequest::new(repo, TreeLimits::default(), DIFF_CAP);
    let ports = AdmitJobPorts {
        fetch: &fetch,
        objects: &objects,
        forge: github,
        ledger: ledger.files,
        refs: ledger.refs,
        bundles: github,
        comments,
        clock: &mut WallClock,
    };
    let Job {
        run,
        output,
        identity,
        plan,
        out,
        document,
        validator,
    } = job;
    let admit_job = AdmitJob {
        run: &run,
        identity: &identity,
        plan: &plan,
        document: &document,
        validator: &validator,
    };
    report(run_admit(admit_job, ports, &out), output, &out)
}

/// Appends admit's outputs for what it wrote, or, when it holds a slot it could not report
/// otherwise, `slot` and `lease_commit` alone.
fn report(
    admitted: Result<AdmitOutputs, error::LiveFailure>,
    output: File,
    out: &OutDir,
) -> Result<String, AdmitCliError> {
    let mut writer = Outputs::new(output);
    let mut write = |outputs: &[(OutputName, String)]| {
        outputs
            .iter()
            .try_for_each(|(name, value)| writer.write(*name, value))
            .map_err(AdmitCliError::Output)
    };
    match admitted {
        Ok(outputs) => {
            write(&outputs.outputs())?;
            Ok(line(out, &outputs))
        }
        Err(error) => {
            if let AdmitRunError::Write {
                held: Some(held), ..
            } = &error
            {
                write(&held.outputs())?;
            }
            Err(AdmitCliError::Admit(Box::new(error)))
        }
    }
}

/// What admit wrote, as one line for the job's log.
fn line(out: &OutDir, outputs: &AdmitOutputs) -> String {
    let held = outputs
        .held()
        .map_or_else(String::new, |held| format!(", slot {}", held.slot));
    format!(
        "assay admit: wrote {} ({}); admission {}{held}",
        out.path().join(RUN_FILE).display(),
        outputs.run_digest(),
        outputs.admission(),
    )
}

/// Fetches pinned commits from the repository with the run token.
struct Fetcher<'a> {
    repo: &'a ObjectRepo,
    remote: &'a HttpsRemote,
    authorization: &'a Authorization,
}

impl FetchCommits for Fetcher<'_> {
    type Error = FetchError;

    fn fetch(&self, commit: &CommitId) -> Result<(), FetchError> {
        let sha: CommitSha = commit.as_str().parse().map_err(FetchError::Commit)?;
        self.repo
            .fetch_authorized(self.remote, self.authorization, &sha)
            .map_err(FetchError::Git)
    }
}

/// The system clock, and a real pause for the slot wait (at most [`assay_app::SLOT_POLLS`] of
/// [`assay_app::SLOT_POLL`]).
struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}
