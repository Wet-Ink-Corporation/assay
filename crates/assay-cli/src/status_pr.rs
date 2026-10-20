//! `assay status [--json] [--pr <n>] [--full]` (#126, #273, 03 §6b): a PR's status read live
//! from GitHub through `gh`, run from the root of a checkout of the repository.
//!
//! The repository is the checkout's `origin`, which must plainly name a github.com repository;
//! without `--pr` the PR is the open one whose head is the checkout's branch. GitHub is read
//! through `gh api` with `ASSAY_GITHUB_TOKEN`, the viewer's own token, and the run bundle a
//! verdict check points at through `gh run download` (wi-6b26d6), both under the shared
//! process-group supervisor. The verdict App and its bot user are named by number in
//! `ASSAY_VERDICT_APP_ID` and `ASSAY_VERDICT_BOT_ID`. The base P10 identity lists are read from
//! `.assay/owners` and `.assay/agents` at `refs/remotes/origin/<the PR's base>`, as CI reads
//! base config at the PR's base (03 §1), through a scratch object repository that borrows the
//! checkout's objects (the checkout is never written); the PR is read once, before them, and
//! the status reports that read. Anything that does not read exactly stops the status
//! (wi-b5affa).

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::num::NonZeroU64;
use std::time::SystemTime;

use assay_app::{
    AppId, BaseIdentitiesError, CalendarDateError, CommitId, ContractSchemaError,
    ContractValidator, FetchedAt, ParseCommitIdError, PullSelector, RubricError, StatusFailure,
    StatusPorts, UserId, base_identities, read_pull, read_rubric,
};
use assay_cli::{
    ForgeObjectsError, ForgePullRequest, ForgeSupervisor, StatusArgs, StatusContext,
    StatusRunError, run_status,
};
use assay_forge::git::{CommitSha, GitEnvironment, GitError, TreeLimits, WorkingTree};
use assay_forge::github::{
    Api, AppIdentity, ForgeError, GhApi, GhEnvironment, GhError, GhSetupError, GhToken, GitHub,
    Repository, ThreadSleep,
};

use crate::check::{
    CheckCliError, DIFF_CAP, Opened, RUBRIC, Scratch, ScratchLeft, TreeVariables, is_branch_name,
    on_path, open_worktree,
};
use crate::doctor::write_chain;

/// The viewer's GitHub token, which every read uses.
pub(crate) const TOKEN: &str = "ASSAY_GITHUB_TOKEN";
/// The `assay-verdict` App's id, which authors its check runs.
pub(crate) const APP_ID: &str = "ASSAY_VERDICT_APP_ID";
/// The id of the App's `[bot]` user, which authors its review comments.
pub(crate) const BOT_ID: &str = "ASSAY_VERDICT_BOT_ID";

/// GitHub as `assay status` reads it: `gh` under the shared supervisor.
type LiveGitHub = GitHub<GhApi<ForgeSupervisor>, ThreadSleep>;

/// The environment a PR's status reads, gathered by `main` (ARC-11).
pub(crate) struct PullVariables {
    /// Where the checkout is and how git runs there; its `PATH` also finds `gh`.
    pub(crate) tree: TreeVariables,
    /// [`TOKEN`].
    pub(crate) token: Option<OsString>,
    /// [`APP_ID`].
    pub(crate) app_id: Option<OsString>,
    /// [`BOT_ID`].
    pub(crate) bot_id: Option<OsString>,
    /// What `gh` inherits: the network path to GitHub and the certificates to trust.
    pub(crate) gh: GhEnvironment,
}

/// What is wrong with one of the variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VariableProblem {
    /// It is not set.
    Missing,
    /// It is set to something it may not hold; its value is never shown.
    Invalid,
}

/// Why a PR's status could not be read.
#[derive(Debug)]
pub(crate) enum PullStatusCliError {
    /// A variable is missing or invalid.
    Variable {
        name: &'static str,
        problem: VariableProblem,
    },
    /// No `gh` on `PATH`.
    NoGh,
    /// The checkout could not be opened or snapshotted.
    Tree(CheckCliError),
    /// git could not read the checkout.
    Git(GitError),
    /// The checkout has no `origin` remote.
    NoOrigin,
    /// `origin` does not plainly name a github.com repository.
    OriginNotGitHub,
    /// HEAD names no branch, and no `--pr` was given.
    Detached,
    /// A commit id git printed is not one the app accepts.
    Commit(ParseCommitIdError),
    /// gh's scratch configuration could not be prepared.
    Gh(GhSetupError),
    /// The repository could not be read from GitHub.
    Repository(ForgeError<GhError>),
    /// The PR could not be read, or not found exactly once.
    Pull(StatusFailure<LiveGitHub, LiveGitHub>),
    /// The PR's base is not a plain branch name.
    BaseRef(String),
    /// `refs/remotes/origin/<the PR's base>` names no commit.
    NoBase(String),
    /// The base identity lists were refused.
    Identities(BaseIdentitiesError<ForgeObjectsError>),
    /// The shipped rubric was refused.
    Rubric(RubricError),
    /// The clock reads a time no status can carry.
    Clock(CalendarDateError),
    /// The contract schemas did not compile.
    Contracts(ContractSchemaError),
    /// The status was refused.
    Status(StatusRunError<StatusFailure<LiveGitHub, LiveGitHub>>),
}

impl fmt::Display for PullStatusCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Variable { name, problem } => {
                let holds = match *name {
                    TOKEN => "the GitHub token assay status reads with",
                    APP_ID => "the assay-verdict App's id, a positive decimal number",
                    _ => "the id of the assay-verdict App's bot user, a positive decimal number",
                };
                match problem {
                    VariableProblem::Missing => write!(f, "{name} is not set: it holds {holds}"),
                    VariableProblem::Invalid => write!(f, "{name} is not {holds}"),
                }
            }
            Self::NoGh => f.write_str("no gh on PATH: assay status reads GitHub through gh"),
            Self::Tree(source) => source.fmt(f),
            Self::Git(source) => write_chain(f, source),
            Self::NoOrigin => f.write_str(
                "the checkout has no origin remote: assay status reads the repository origin \
                 names",
            ),
            Self::OriginNotGitHub => f.write_str(
                "the origin remote does not plainly name a github.com repository \
                 (https://github.com/<owner>/<name> or git@github.com:<owner>/<name>)",
            ),
            Self::Detached => {
                f.write_str("HEAD is detached, so it names no PR: pass --pr <number>")
            }
            Self::Commit(source) => write_chain(f, source),
            Self::Gh(source) => write_chain(f, source),
            Self::Repository(source) => {
                f.write_str("could not read the repository: ")?;
                write_chain(f, source)
            }
            Self::Pull(source) => {
                f.write_str("no status: ")?;
                write_chain(f, source)
            }
            Self::BaseRef(name) => write!(
                f,
                "the PR's base {name:?} is not a plain branch name, so its owner and agent \
                 lists are not read"
            ),
            Self::NoBase(name) => write!(
                f,
                "refs/remotes/origin/{name}, the PR's base, is not a commit: fetch it (the owner \
                 and agent lists are read there)"
            ),
            Self::Identities(source) => write_chain(f, source),
            Self::Rubric(source) => write_chain(f, source),
            Self::Clock(source) => write!(f, "the system clock: {source}"),
            Self::Contracts(source) => write_chain(f, source),
            Self::Status(source) => write_chain(f, source),
        }
    }
}

impl std::error::Error for PullStatusCliError {}

/// gh's scratch configuration directory could not be removed.
#[derive(Debug)]
pub(crate) struct GhLeft(io::Error);

impl fmt::Display for GhLeft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not remove gh's scratch configuration directory: {}",
            self.0
        )
    }
}

/// How a status that reached GitHub ended: the status or why there is none, and what could not
/// be removed afterwards.
pub(crate) struct Ended {
    /// The status as `args` asked for it, to be written to stdout.
    pub(crate) status: Result<Vec<u8>, PullStatusCliError>,
    /// gh's scratch configuration directory, if it could not be removed.
    pub(crate) gh_left: Option<GhLeft>,
    /// The scratch object repository, if it could not be removed.
    pub(crate) scratch_left: Option<ScratchLeft>,
}

/// Reads the status of the PR `args` name from GitHub, for the checkout `variables` name.
///
/// # Errors
///
/// [`PullStatusCliError`] for anything that stops it before gh's scratch directory exists:
/// a variable, `gh`, the checkout, its origin and branch.
pub(crate) fn run(
    args: &StatusArgs,
    variables: PullVariables,
) -> Result<Ended, PullStatusCliError> {
    let token = token(variables.token)?;
    let identity = AppIdentity {
        app: AppId(id(APP_ID, variables.app_id)?),
        bot: UserId(id(BOT_ID, variables.bot_id)?),
    };
    let program = on_path("gh", variables.tree.path.as_ref()).ok_or(PullStatusCliError::NoGh)?;
    let Opened {
        worktree,
        git,
        killer,
    } = open_worktree(variables.tree).map_err(PullStatusCliError::Tree)?;
    let url = worktree
        .origin_url()
        .map_err(PullStatusCliError::Git)?
        .ok_or(PullStatusCliError::NoOrigin)?;
    // The URL is never shown: it may carry credentials.
    let repository = Repository::from_remote_url(&url)
        .map_err(|_not_github| PullStatusCliError::OriginNotGitHub)?;
    let head = worktree.resolve("HEAD").map_err(PullStatusCliError::Git)?;
    // The branch is read only when it names the PR.
    let branch;
    let pull = if let Some(number) = args.pr {
        PullSelector::Number(number)
    } else {
        branch = worktree
            .branch()
            .map_err(PullStatusCliError::Git)?
            .ok_or(PullStatusCliError::Detached)?;
        PullSelector::Branch(&branch)
    };
    let supervisor = ForgeSupervisor::new(killer);
    let gh =
        GhApi::new(supervisor, program, token, variables.gh).map_err(PullStatusCliError::Gh)?;
    let github = GitHub::new(Api::new(gh, ThreadSleep), repository, identity);
    let scratch = match Scratch::new() {
        Ok(scratch) => scratch,
        Err(error) => {
            return Ok(Ended {
                status: Err(PullStatusCliError::Tree(error)),
                gh_left: close_gh(github),
                scratch_left: None,
            });
        }
    };
    let checkout = Checkout {
        worktree: &worktree,
        git,
        head: &head,
        pull,
    };
    let status = read(args, (&github, identity.app), &scratch, checkout);
    Ok(Ended {
        status,
        gh_left: close_gh(github),
        scratch_left: scratch.close(),
    })
}

/// The checkout as the status reads it.
struct Checkout<'a> {
    worktree: &'a WorkingTree,
    git: GitEnvironment,
    head: &'a CommitSha,
    /// The PR: `--pr`'s number, or the checkout branch's open PR.
    pull: PullSelector<'a>,
}

/// The status of the PR `checkout` names, read through `github` as the App `app` publishes
/// it, with the base identity lists read at `refs/remotes/origin/<the PR's base>` (what CI
/// reads them at, 03 §1) from a snapshot repository in `scratch` that borrows the checkout's
/// objects. The PR is read once, and the status reports that read.
fn read(
    args: &StatusArgs,
    (github, app): (&LiveGitHub, AppId),
    scratch: &Scratch,
    checkout: Checkout<'_>,
) -> Result<Vec<u8>, PullStatusCliError> {
    let repository = github
        .status_repository()
        .map_err(PullStatusCliError::Repository)?;
    let pull = read_pull(checkout.pull, github).map_err(PullStatusCliError::Pull)?;
    if !is_branch_name(&pull.base_ref) {
        return Err(PullStatusCliError::BaseRef(pull.base_ref));
    }
    let base = base_commit(checkout.worktree, &pull.base_ref)?;
    let (repo, _snapshot) = scratch
        .snapshot(checkout.worktree, checkout.head, checkout.git)
        .map_err(PullStatusCliError::Tree)?;
    let objects = ForgePullRequest::new(&repo, TreeLimits::default(), DIFF_CAP);
    let identities = base_identities(&objects, &base).map_err(PullStatusCliError::Identities)?;
    let rubric = read_rubric(RUBRIC).map_err(PullStatusCliError::Rubric)?;
    let local_head = commit_id(checkout.head)?;
    let fetched_at =
        FetchedAt::from_system_time(SystemTime::now()).map_err(PullStatusCliError::Clock)?;
    let validator = ContractValidator::new().map_err(PullStatusCliError::Contracts)?;
    let context = StatusContext {
        repository: &repository,
        app,
        pull: PullSelector::Read(&pull),
        local_head: Some(&local_head),
        rubric: &rubric,
        identities: &identities,
        fetched_at: &fetched_at,
        validator: &validator,
    };
    let ports = StatusPorts {
        forge: github,
        bundles: github,
    };
    let mut bytes = Vec::new();
    run_status(args, &context, &ports, &mut bytes).map_err(PullStatusCliError::Status)?;
    Ok(bytes)
}

/// The commit of `refs/remotes/origin/<branch>`, where the identity lists are read.
fn base_commit(worktree: &WorkingTree, branch: &str) -> Result<CommitId, PullStatusCliError> {
    match worktree.resolve(&["refs/remotes/origin/", branch].concat()) {
        Ok(base) => commit_id(&base),
        // The error owns the branch it names.
        Err(GitError::UnknownRevision { .. }) => Err(PullStatusCliError::NoBase(branch.to_owned())),
        Err(error) => Err(PullStatusCliError::Git(error)),
    }
}

/// forge's commit id as the app's.
fn commit_id(sha: &CommitSha) -> Result<CommitId, PullStatusCliError> {
    sha.as_str().parse().map_err(PullStatusCliError::Commit)
}

/// Closes gh's transport, saying so when its directory is left.
fn close_gh(github: LiveGitHub) -> Option<GhLeft> {
    github.into_api().into_transport().close().err().map(GhLeft)
}

/// [`TOKEN`] as a token.
fn token(value: Option<OsString>) -> Result<GhToken, PullStatusCliError> {
    let refused = |problem| PullStatusCliError::Variable {
        name: TOKEN,
        problem,
    };
    let value = value.ok_or_else(|| refused(VariableProblem::Missing))?;
    let text = value
        .into_string()
        .map_err(|_not_utf8| refused(VariableProblem::Invalid))?;
    GhToken::new(text).map_err(|_invalid| refused(VariableProblem::Invalid))
}

/// The variable `name` as an id: ASCII digits only, not zero.
fn id(name: &'static str, value: Option<OsString>) -> Result<NonZeroU64, PullStatusCliError> {
    let refused = |problem| PullStatusCliError::Variable { name, problem };
    let value = value.ok_or_else(|| refused(VariableProblem::Missing))?;
    value
        .to_str()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| refused(VariableProblem::Invalid))
}
