//! `assay decide --config <plugins-toml> --artifacts <dir> --out <dir>` (#60 ST-14, Z4, D5 A):
//! the decide job's first step.
//!
//! In order, everything it can refuse before a token is read: its flags; the run, refused for
//! any attempt but the first (R3-05, I1); the regular file `GITHUB_OUTPUT` names (opened, never
//! created); every upstream job's outputs from `ASSAY_NEEDS` (I2, I6); `--out` as an
//! [`OutDir`]; the contract schemas, the embedded prompt bundle and rubric v1. Then the ledger
//! repository from base config's `[assay].ledger_repo` (D3 A), the three tokens and the verdict
//! App's ids: `ASSAY_LEDGER_TOKEN` (the ledger's files, [`crate::ledger_job`]),
//! `ASSAY_GITHUB_TOKEN` (the `assay-verdict` App's: reviews, the range's commit identities,
//! labels, reactions, the timeline and the command records' comments) and `ASSAY_RUN_TOKEN`
//! (the job's `github.token`: the PR's live state, its REST file list, the pinned commits
//! fetched into an object-only repository in a scratch directory, and the previous run's
//! bundle). The artifacts are read from `<artifacts>/<name>/` as `download-artifact` lays them
//! out ([`DirArtifacts`]). [`run_decide`] writes the state and, for a decided run, the bundle
//! into `--out`; decide's outputs (03 §4a) are appended after. Nothing is published here. A
//! variable's value is never shown.

mod error;

use std::ffi::OsString;
use std::fs::File;
use std::path::PathBuf;

use assay_app::{ContractValidator, RestFiles, read_rubric};
use assay_cli::outputs::{DECIDE_NEEDS, DecideNeeds, DecideOutputs};
use assay_cli::{
    DecideJob, DecideJobPorts, DirArtifacts, FirstAttempt, ForgePullRequest, Needs, OutDir,
    Outputs, RunEnv, RunEnvError, RunVariables, STATE_FILE, run_decide,
};
use assay_domain::{ContentHash, Rubric};
use assay_forge::git::{
    Authorization, GitEnvironment, GitLimits, HttpsRemote, ObjectRepo, TreeLimits,
};
use assay_forge::github::{
    Api, AppIdentity, GhEnvironment, GhToken, GitHub, Repository, ThreadSleep,
};
use assay_forge::process::GroupKiller;

pub(crate) use self::error::{Common, DecideCliError, PublishCliError, Variable};
use crate::admit_job::Fetcher;
use crate::bundle;
use crate::check::{DIFF_CAP, RUBRIC};
use crate::guard_job::{ACTIONS, open_output};
use crate::ledger_job::{
    self, Gh, LedgerJob, LedgerPorts, LedgerTransport, Left, Problem, Programs, ledger_repo,
};

/// The decide job, as its refusals name it.
const DECIDE: LedgerJob = LedgerJob {
    name: "decide",
    purpose: "decide reads the PR's ledger state",
};

/// The flags, in the order `decide` takes them.
const CONFIG: &str = "--config";
const ARTIFACTS: &str = "--artifacts";
const OUT: &str = "--out";

/// The environment `assay decide` reads, gathered by `main` (ARC-11).
pub(crate) struct DecideVariables {
    /// The run.
    pub(crate) run: RunVariables,
    /// [`Variable::Output`].
    pub(crate) output: Option<OsString>,
    /// The tokens and ids both commands read.
    pub(crate) common: CommonVariables,
}

/// What `decide` and `publish` both read, gathered by `main` (ARC-11).
pub(crate) struct CommonVariables {
    /// [`Variable::Needs`].
    pub(crate) needs: Option<OsString>,
    /// [`Variable::Token`].
    pub(crate) token: Option<OsString>,
    /// [`Variable::LedgerToken`].
    pub(crate) ledger_token: Option<OsString>,
    /// [`Variable::RunToken`].
    pub(crate) run_token: Option<OsString>,
    /// [`Variable::AppId`].
    pub(crate) app_id: Option<OsString>,
    /// [`Variable::BotId`].
    pub(crate) bot_id: Option<OsString>,
    /// `PATH`, which finds `gh` and git.
    pub(crate) path: Option<OsString>,
    /// What `gh` inherits.
    pub(crate) gh: GhEnvironment,
}

/// How `assay decide` ended: what decide wrote, as one line, or why it did not.
pub(crate) type Ended = ledger_job::Ended<DecideCliError>;

/// The three paths `decide` takes.
struct Args {
    config: PathBuf,
    artifacts: PathBuf,
    out: PathBuf,
}

/// What the decision itself needs, read before any port exists.
struct Job {
    run: FirstAttempt,
    output: File,
    needs: DecideNeeds,
    artifacts: DirArtifacts,
    out: OutDir,
    prompt_bundle: ContentHash,
    rubric: Rubric,
    validator: ContractValidator,
}

/// The tokens, each for its own transport, and the verdict App's identity.
pub(crate) struct Tokens {
    /// The `assay-verdict` App's token.
    pub(crate) app: GhToken,
    /// The `assay-ledger-writer` App's token.
    pub(crate) ledger: GhToken,
    /// The job's `github.token`.
    pub(crate) run: GhToken,
    /// The `assay-verdict` App and its bot user.
    pub(crate) identity: AppIdentity,
}

/// Everything decide reads before it starts a process.
struct Prepared {
    job: Job,
    ledger: Repository,
    tokens: Tokens,
    /// The run token as git's header, and where git sends it.
    fetch: (HttpsRemote, Authorization),
}

/// Runs `assay decide` with `args` (after `decide`) and `variables`.
pub(crate) fn run(args: impl Iterator<Item = OsString>, variables: DecideVariables) -> Ended {
    let mut left = Vec::new();
    let line =
        prepare(args, &variables).and_then(|prepared| connect(prepared, variables, &mut left));
    Ended { line, left }
}

/// `--config <plugins-toml> --artifacts <dir> --out <dir>`, exactly.
fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Args, DecideCliError> {
    let mut flag = |name: &str| match (args.next(), args.next()) {
        (Some(flag), Some(value)) if flag == name => Ok(PathBuf::from(value)),
        _ => Err(DecideCliError::Usage),
    };
    let parsed = Args {
        config: flag(CONFIG)?,
        artifacts: flag(ARTIFACTS)?,
        out: flag(OUT)?,
    };
    match args.next() {
        None => Ok(parsed),
        Some(_) => Err(DecideCliError::Usage),
    }
}

/// The run as its first attempt (I1): read before anything else in the environment.
pub(crate) fn first_attempt(variables: &RunVariables) -> Result<FirstAttempt, Common> {
    RunEnv::from_vars(variables)
        .and_then(|run| run.first_attempt().map_err(RunEnvError::from))
        .map_err(Common::Run)
}

/// Every upstream job's outputs, read as decide reads them.
pub(crate) fn decide_needs(value: Option<&OsString>) -> Result<DecideNeeds, Common> {
    let needs = present(value, Variable::Needs)?;
    let needs = Needs::parse(needs.as_encoded_bytes(), &DECIDE_NEEDS).map_err(Common::Needs)?;
    DecideNeeds::from_needs(&needs).map_err(Common::Needs)
}

/// The three tokens and the verdict App's ids; their values are never shown.
pub(crate) fn tokens(variables: &CommonVariables) -> Result<Tokens, Common> {
    let refused = |variable| move |problem| Common::Variable { variable, problem };
    let token = |variable, value: Option<&OsString>| {
        ledger_job::token(value.map(OsString::as_os_str)).map_err(refused(variable))
    };
    let id = |variable, value: Option<&OsString>| {
        ledger_job::id(value.map(OsString::as_os_str)).map_err(refused(variable))
    };
    Ok(Tokens {
        app: token(Variable::Token, variables.token.as_ref())?,
        ledger: token(Variable::LedgerToken, variables.ledger_token.as_ref())?,
        run: token(Variable::RunToken, variables.run_token.as_ref())?,
        identity: AppIdentity {
            app: assay_app::AppId(id(Variable::AppId, variables.app_id.as_ref())?),
            bot: assay_app::UserId(id(Variable::BotId, variables.bot_id.as_ref())?),
        },
    })
}

/// Everything decide can refuse before it starts a process, the arguments and the run first.
fn prepare(
    args: impl Iterator<Item = OsString>,
    variables: &DecideVariables,
) -> Result<Prepared, DecideCliError> {
    let args = parse(args)?;
    let run = first_attempt(&variables.run).map_err(DecideCliError::Common)?;
    let output =
        present(variables.output.as_ref(), Variable::Output).map_err(DecideCliError::Common)?;
    let output = open_output(output).map_err(DecideCliError::OutputFile)?;
    let needs = decide_needs(variables.common.needs.as_ref()).map_err(DecideCliError::Common)?;
    let out = OutDir::new(&args.out).map_err(DecideCliError::Out)?;
    let validator = ContractValidator::new().map_err(DecideCliError::Validator)?;
    let prompt_bundle = bundle::embedded().map_err(DecideCliError::Bundle)?.hash();
    let rubric =
        read_rubric(RUBRIC).map_err(|error| DecideCliError::Common(Common::Rubric(error)))?;
    let ledger = ledger_repo(DECIDE, &args.config)
        .map_err(|error| DecideCliError::Common(Common::Setup(error)))?;
    let tokens = tokens(&variables.common).map_err(DecideCliError::Common)?;
    // git authenticates with the run token, sent to the repository's URL only.
    let authorization = tokens
        .run
        .git_authorization()
        .map_err(DecideCliError::Authorization)?;
    let remote = format!("https://github.com/{}.git", run.run_env().repo)
        .parse()
        .map_err(DecideCliError::Remote)?;
    Ok(Prepared {
        job: Job {
            run,
            output,
            needs,
            artifacts: DirArtifacts::new(args.artifacts),
            out,
            prompt_bundle,
            rubric,
            validator,
        },
        ledger,
        tokens,
        fetch: (remote, authorization),
    })
}

/// `value`, which must be set.
pub(crate) fn present(value: Option<&OsString>, variable: Variable) -> Result<&OsString, Common> {
    value.ok_or(Common::Variable {
        variable,
        problem: Problem::Missing,
    })
}

/// Builds the ledger's transport, decides over it, and removes the directories, pushing onto
/// `left` each one that could not be removed.
fn connect(
    prepared: Prepared,
    variables: DecideVariables,
    left: &mut Vec<Left>,
) -> Result<String, DecideCliError> {
    let Prepared {
        job,
        ledger,
        tokens,
        fetch,
    } = prepared;
    let common = variables.common;
    // git's environment keeps its own copy of PATH; the ledger's transport takes the other.
    let git_path = common.path.clone().ok_or(DecideCliError::NoGit)?;
    let programs = Programs {
        path: common.path,
        gh: common.gh,
    };
    let Tokens {
        app,
        ledger: ledger_token,
        run,
        identity,
    } = tokens;
    LedgerTransport::open(DECIDE, (ledger, ledger_token), programs, left)
        .map_err(|error| DecideCliError::Common(Common::Setup(error)))?
        .run(left, |ports, left| {
            let repos = Repos { git_path, fetch };
            decide_over(job, ((app, identity), run), (&repos, &ports), left)
        })
}

/// What the object repository is built and fetched with.
struct Repos {
    git_path: OsString,
    fetch: (HttpsRemote, Authorization),
}

/// The two GitHub adapters (the App's, `app`, for what the snapshot reads; the run token's,
/// `run`, for the live state, the file list and bundles), then the decision in a scratch
/// directory; each transport and directory is closed after, and pushed onto `left` when it
/// cannot be.
fn decide_over(
    job: Job,
    ((app, identity), run): ((GhToken, AppIdentity), GhToken),
    (repos, ledger): (&Repos, &LedgerPorts<'_, '_>),
    left: &mut Vec<Left>,
) -> Result<String, DecideCliError> {
    let setup = |error| DecideCliError::Common(Common::Setup(error));
    // Each adapter stores the repository; the run keeps its own for the job.
    let repository = job.run.run_env().repo.clone();
    let app_gh = ledger.gh.transport(app).map_err(setup)?;
    let verdict = GitHub::new(Api::new(app_gh, ThreadSleep), repository.clone(), identity);
    let decided = ledger.gh.transport(run).map_err(setup).and_then(|run_gh| {
        // No read through the run token is attributed to an App.
        let github = GitHub::new(Api::new(run_gh, ThreadSleep), repository, ACTIONS);
        let decided = github
            .pull_files(job.needs.identity.pr)
            .map_err(DecideCliError::Files)
            .and_then(|files| {
                let live = Live {
                    repos,
                    ledger,
                    github: (&github, &verdict),
                };
                in_scratch(job, files, &live, left)
            });
        close(github, Variable::RunToken.name(), left);
        decided
    });
    close(verdict, Variable::Token.name(), left);
    decided
}

/// Closes `github`'s transport, pushing its directory onto `left` when it cannot be removed.
pub(crate) fn close(github: GitHub<Gh, ThreadSleep>, token: &'static str, left: &mut Vec<Left>) {
    if let Err(error) = github.into_api().into_transport().close() {
        left.push(Left::Gh(token, error));
    }
}

/// What the decision reads through: the object repository's setup, the ledger, and the run
/// token's and the App's GitHub.
struct Live<'a, 'p, 'r> {
    repos: &'a Repos,
    ledger: &'a LedgerPorts<'p, 'r>,
    github: (&'a GitHub<Gh, ThreadSleep>, &'a GitHub<Gh, ThreadSleep>),
}

/// A scratch directory for the object repository, the decision in it, and the directory
/// removed after, pushed onto `left` when it cannot be.
fn in_scratch(
    job: Job,
    rest_files: RestFiles,
    live: &Live<'_, '_, '_>,
    left: &mut Vec<Left>,
) -> Result<String, DecideCliError> {
    let killer = GroupKiller::system().map_err(DecideCliError::Killer)?;
    let scratch = tempfile::tempdir().map_err(DecideCliError::Scratch)?;
    // The environment stores its own PATH.
    let git = GitEnvironment::new(live.repos.git_path.clone(), killer);
    let decided = ObjectRepo::init(
        scratch.path().join("objects.git"),
        git,
        GitLimits::default(),
    )
    .map_err(DecideCliError::Git)
    .and_then(|repo| decide_in(job, rest_files, live, &repo));
    if let Err(error) = scratch.close() {
        left.push(Left::Scratch(DECIDE.name, error));
    }
    decided
}

/// The decision over `repo`, then the outputs.
fn decide_in(
    job: Job,
    rest_files: RestFiles,
    live: &Live<'_, '_, '_>,
    repo: &ObjectRepo,
) -> Result<String, DecideCliError> {
    let (remote, authorization) = &live.repos.fetch;
    let (github, verdict) = live.github;
    let fetch = Fetcher {
        repo,
        remote,
        authorization,
    };
    let objects = ForgePullRequest::new(repo, TreeLimits::default(), DIFF_CAP);
    let Job {
        run,
        output,
        needs,
        artifacts,
        out,
        prompt_bundle,
        rubric,
        validator,
    } = job;
    let ports = DecideJobPorts {
        fetch: &fetch,
        objects: &objects,
        state: github,
        artifacts: &artifacts,
        forge: verdict,
        ledger: live.ledger.files,
        bundles: github,
        comments: verdict,
    };
    let decide_job = DecideJob {
        run: &run,
        needs,
        rest_files,
        prompt_bundle,
        rubric: &rubric,
        validator: &validator,
    };
    let outputs = run_decide(decide_job, &ports, &out)
        .map_err(|error| DecideCliError::Decide(Box::new(error)))?;
    let mut writer = Outputs::new(output);
    for (name, value) in outputs.outputs() {
        writer.write(name, &value).map_err(DecideCliError::Output)?;
    }
    Ok(line(&out, &outputs))
}

/// What decide wrote, as one line for the job's log.
fn line(out: &OutDir, outputs: &DecideOutputs) -> String {
    let state = out.path().join(STATE_FILE);
    match outputs {
        DecideOutputs::NoBundle => {
            format!("assay decide: wrote {}; no bundle", state.display())
        }
        DecideOutputs::Bundle { digest } => format!(
            "assay decide: wrote {} and {} ({digest})",
            state.display(),
            out.path().join(assay_app::BUNDLE_FILE).display(),
        ),
    }
}
