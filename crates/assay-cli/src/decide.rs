//! The pipeline's `decide` job (#60 ST-14, Z4, 04 §5, D5 A): the run's inputs authenticated
//! through [`assay_app::authenticate`]; for an authenticated run, the GitHub state snapshot, the
//! decision inputs and the `assay.run-inputs/1` bundle built by [`assay_app::prepare`]; and
//! what `assay publish` publishes, as [`DecideState`].
//!
//! [`run_decide`] creates `--out` and writes the state as [`STATE_FILE`] and, for a decided
//! run only, the bundle as [`assay_app::BUNDLE_FILE`], then returns decide's outputs (03 §4a).
//! Nothing is published here: no check, comment or ledger write.
//!
//! The plan is recomputed from the request `plan` built ([`crate::plan_request`]). The pinned
//! commits are fetched only for a plan that succeeded, the one case `authenticate` reads them.
//! For an authenticated run, in order: the base P10 lists ([`assay_app::base_lists`]); the PR's
//! ledger state and the carry decided on its evidence pointer, as `admit` decides it; the
//! previous run's carried findings and waivers ([`assay_app::prior_carry`],
//! [`assay_app::prior_waivers`]); the snapshot ([`assay_app::github_state`]) over the ledger's
//! push records ([`assay_app::LedgerSnapshot`]); the decision inputs
//! ([`assay_app::decision_inputs`]) with guard's draft flag and the ledger's requeue count of
//! this head; and the carry set, unjudged ([`assay_app::CarrySet::unjudged`]): until the real
//! review (M2) no stage judges a carried finding. The bundle records no event (D7 is open).

mod error;
mod state;

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;

use assay_app::{
    ArtifactJob, AuthenticatedRun, Authentication, BUNDLE_FILE, BundleSource, CarriedFinding,
    Carry, CarrySet, CommandComments, ContractValidator, DecidePorts, DecideRequest, Decided,
    EligibilityLists, Forge, GitHubState, InputsRequest, JobResult, Ledger, LedgerSnapshot,
    PlanJob, PreparedBundle, PriorCarry, PriorWaivers, PullRequestState, RefusalReason, RestFiles,
    RunArtifacts, Sha256Digest, StatePorts, StateRequest, Target, authenticate, base_lists,
    decision_inputs, github_state, prepare, prior_carry, prior_waivers,
};
use assay_domain::{ContentHash, Readiness, Rubric};

pub use self::error::{DecideRunError, DecideRunFailure};
pub use self::state::{DecideState, DecideStateError, MAX_STATE_BYTES, STATE_FILE};
use crate::admit::{FetchCommits, FetchedSide};
use crate::ci::{FirstAttempt, OutDir, RunEnv};
use crate::outputs::{DecideNeeds, DecideOutputs, Identity, PlanEnd, PlanOutputs};
use crate::plan::plan_request;
use crate::prior::prior;
use crate::pull_request::ForgePullRequest;

/// What `decide` is given besides its ports.
#[derive(Debug)]
pub struct DecideJob<'a> {
    /// The run, checked to be its first attempt.
    pub run: &'a FirstAttempt,
    /// What decide read of its upstream jobs, moved into the decide request.
    pub needs: DecideNeeds,
    /// GitHub's list of the PR's files, read as `plan` read it, moved into the plan request.
    pub rest_files: RestFiles,
    /// The embedded prompt bundle's hash, as `plan` put it in the evidence key.
    pub prompt_bundle: ContentHash,
    /// Rubric v1.
    pub rubric: &'a Rubric,
    /// The contract validator the documents are checked with.
    pub validator: &'a ContractValidator,
}

/// What `decide` reads through, borrowed for one call.
#[derive(Debug)]
pub struct DecideJobPorts<'p, 'r, H, S, A, F, L, B, C> {
    /// Fetches the pinned commits into [`Self::objects`]' repository.
    pub fetch: &'p H,
    /// The PR's objects.
    pub objects: &'p ForgePullRequest<'r>,
    /// GitHub's live state of the PR.
    pub state: &'p S,
    /// The run's downloaded artifacts.
    pub artifacts: &'p A,
    /// GitHub, for the reviews and the range's commit identities.
    pub forge: &'p F,
    /// The ledger's files.
    pub ledger: &'p L,
    /// The run artifacts of earlier runs, for the previous run's bundle.
    pub bundles: &'p B,
    /// The PR's issue comments, behind command records.
    pub comments: &'p C,
}

/// What [`run_decide`] returns over the ports' error types.
pub type DecideRunOutcome<H, S, A, F, L, B, C> =
    Result<DecideOutputs, DecideRunFailure<H, S, A, F, L, B, C>>;

/// A step of [`run_decide`] over the ports' error types.
type Step<T, H, S, A, F, L, B, C> = Result<T, DecideRunFailure<H, S, A, F, L, B, C>>;

/// Decides `job`'s run through `ports` and writes what `publish` publishes into `out`.
///
/// It creates `out` and writes [`STATE_FILE`]: [`DecideState::Silent`] for a superseded or
/// yielding run, [`DecideState::Refused`] with the refusal's reason, or, for an authenticated
/// run, [`DecideState::Decided`] after the bundle as [`BUNDLE_FILE`], the exact bytes
/// [`prepare`] built, whose sha256 it names.
///
/// # Errors
///
/// [`DecideRunError`] when a port fails or the inputs, the bundle or `out` cannot be built or
/// written; a refusal is a [`DecideState`], not an error. Nothing is written into `out` before
/// everything it holds is ready.
pub fn run_decide<H, S, A, F, L, B, C>(
    job: DecideJob<'_>,
    ports: &DecideJobPorts<'_, '_, H, S, A, F, L, B, C>,
    out: &OutDir,
) -> DecideRunOutcome<H, S, A, F, L, B, C>
where
    H: FetchCommits,
    S: PullRequestState,
    A: RunArtifacts,
    F: Forge,
    L: Ledger<Error: 'static>,
    B: BundleSource,
    C: CommandComments,
{
    let plan_job = match &job.needs.plan {
        PlanEnd::Succeeded(outputs) => {
            fetch_pinned(ports.fetch, &job.needs.identity)
                .map_err(|(side, source)| DecideRunError::Fetch { side, source })?;
            plan_job(outputs).ok_or(DecideRunError::NoPlanArtifact)?
        }
        // Not read: `authenticate` refuses a plan that did not succeed before it reads the
        // artifact.
        PlanEnd::NotSucceeded(result) => PlanJob {
            result: (*result).into(),
            artifact: String::new(),
            document: Sha256Digest::of(&[]),
        },
    };
    let run_env = job.run.run_env();
    let (request, identity) = decide_request(
        run_env,
        job.needs,
        (job.rest_files, job.prompt_bundle),
        plan_job,
    );
    let decide_ports = DecidePorts {
        objects: ports.objects,
        state: ports.state,
        artifacts: ports.artifacts,
    };
    let authentication = authenticate(&request, &decide_ports, job.validator)
        .map_err(DecideRunError::Authenticate)?;
    let (state, bundle) = match authentication {
        Authentication::Superseded(_) | Authentication::YieldToNewer => (DecideState::Silent, None),
        Authentication::Refused(refusal) => {
            (DecideState::Refused(RefusalReason::from(&refusal)), None)
        }
        Authentication::Authenticated(authenticated) => {
            let accepted = Accepted {
                run_env,
                identity: &identity,
                rubric: job.rubric,
                request: &request,
                authenticated: &authenticated,
            };
            let bundle = bundle(&accepted, ports)?;
            // The state owns its copy of the digest; the bundle's bytes are written beside it.
            let digest = bundle.digest().clone();
            (DecideState::Decided { digest }, Some(bundle))
        }
    };
    write_out(out, &state, bundle.as_ref().map(PreparedBundle::bytes))
        .map_err(|(path, source)| DecideRunError::Write { path, source })?;
    Ok(match state {
        DecideState::Decided { digest } => DecideOutputs::Bundle { digest },
        DecideState::Silent | DecideState::Refused(_) => DecideOutputs::NoBundle,
    })
}

/// The decide request for `needs` in `run`, over GitHub's file list and the prompt bundle's
/// hash (`plan`) and plan's job (`plan_job`), and guard's identity, which the snapshot reads.
fn decide_request(
    run: &RunEnv,
    needs: DecideNeeds,
    (rest_files, prompt_bundle): (RestFiles, ContentHash),
    plan_job: PlanJob,
) -> (DecideRequest, Identity) {
    let DecideNeeds {
        identity,
        plan: _,
        gates_static,
        gates_lint,
        gates_test,
        admit,
        review,
        release,
    } = needs;
    let request = DecideRequest {
        // The request owns the identity's PR; decide keeps the identity for the snapshot.
        plan: plan_request(run, identity.clone(), rest_files, prompt_bundle),
        plan_job,
        gates_static,
        gates_lint,
        gates_test,
        admit,
        review,
        release,
    };
    (request, identity)
}

/// The `plan` job as `decide` reads it from plan's `outputs`: its artifact and the digest of
/// its `run.json`; none when the outputs name no plan artifact.
fn plan_job(outputs: &PlanOutputs) -> Option<PlanJob> {
    // The request owns its copy of the name and of the digest; `outputs` stays with the needs.
    let artifact = outputs.artifacts().get(&ArtifactJob::Plan).cloned()?;
    Some(PlanJob {
        result: JobResult::Success,
        artifact,
        document: outputs.run_digest().clone(),
    })
}

/// Fetches the pinned base and head; the side that could not be fetched, and why.
fn fetch_pinned<H: FetchCommits>(
    fetch: &H,
    identity: &Identity,
) -> Result<(), (FetchedSide, H::Error)> {
    for (side, commit) in [
        (FetchedSide::Base, &identity.base_sha),
        (FetchedSide::Head, &identity.head_sha),
    ] {
        fetch.fetch(commit).map_err(|source| (side, source))?;
    }
    Ok(())
}

/// An authenticated run and what `decide` was given for it.
struct Accepted<'a> {
    run_env: &'a RunEnv,
    identity: &'a Identity,
    rubric: &'a Rubric,
    request: &'a DecideRequest,
    authenticated: &'a AuthenticatedRun,
}

/// The bundle of the authenticated run: the snapshot, the inputs and the carry set, built
/// into `assay.run-inputs/1` by [`prepare`].
fn bundle<H, S, A, F, L, B, C>(
    accepted: &Accepted<'_>,
    ports: &DecideJobPorts<'_, '_, H, S, A, F, L, B, C>,
) -> Step<PreparedBundle, H, S, A, F, L, B, C>
where
    H: FetchCommits,
    S: PullRequestState,
    A: RunArtifacts,
    F: Forge,
    L: Ledger<Error: 'static>,
    B: BundleSource,
    C: CommandComments,
{
    let (identity, request, run_env) = (accepted.identity, accepted.request, accepted.run_env);
    let lists = base_lists(ports.objects, &identity.base_sha).map_err(DecideRunError::Lists)?;
    let prior = prior(
        (run_env, identity),
        ports.objects,
        ports.ledger,
        ports.bundles,
    )?;
    let requeues = prior
        .state
        .deferral
        .as_ref()
        .filter(|deferral| deferral.head == identity.head_sha)
        .map_or(0, |deferral| deferral.count);
    let repository = run_env.repo.to_string();
    let previous = prior.state.evidence.as_ref().map(|pointer| PriorCarry {
        repo_id: run_env.run_uid.repo_id,
        pointer,
        repository: &repository,
    });
    let (carried, waivers) = carried(prior.carry, previous.as_ref(), ports.bundles)?;
    let github = snapshot(accepted, ports, &lists)?;
    let inputs = decision_inputs(&InputsRequest {
        run: accepted.authenticated,
        state: &github,
        readiness: if identity.draft {
            Readiness::Draft
        } else {
            Readiness::Ready
        },
        requeues,
        prior: &waivers,
        admit: request.admit,
        review: &request.review,
    })
    .map_err(DecideRunError::Inputs)?;
    let target = Target {
        run_uid: run_env.run_uid,
        pull_request: &request.plan.pull_request,
        default_branch: &identity.default_branch,
    };
    prepare(
        &target,
        &Decided {
            run: accepted.authenticated,
            rubric: accepted.rubric,
            inputs: &inputs,
            // D7 is open: decide's events are not part of the bundle yet.
            events: &[],
            carry: &CarrySet::unjudged(carried),
            github: &github,
            cost: request.review.cost,
        },
    )
    .map_err(DecideRunError::Prepare)
}

/// The GitHub state snapshot of the authenticated run's PR, judged by the base `lists`.
fn snapshot<H, S, A, F, L, B, C>(
    accepted: &Accepted<'_>,
    ports: &DecideJobPorts<'_, '_, H, S, A, F, L, B, C>,
    lists: &EligibilityLists,
) -> Step<GitHubState, H, S, A, F, L, B, C>
where
    H: FetchCommits,
    S: PullRequestState,
    A: RunArtifacts,
    F: Forge,
    L: Ledger<Error: 'static>,
    B: BundleSource,
    C: CommandComments,
{
    let identity = accepted.identity;
    github_state(
        &StatePorts {
            forge: ports.forge,
            objects: ports.objects,
            ledger: &LedgerSnapshot {
                ledger: ports.ledger,
                repo_id: accepted.run_env.run_uid.repo_id,
            },
            comments: ports.comments,
        },
        &StateRequest {
            number: identity.pr,
            author: identity.author,
            base_sha: &identity.base_sha,
            head_sha: &identity.head_sha,
            lists,
        },
    )
    .map_err(|error| DecideRunError::Snapshot(Box::new(error)))
}

/// What [`carried`] returns over the bundle store's error.
type Carried<H, S, A, F, L, B, C> =
    Result<(Vec<CarriedFinding>, PriorWaivers), DecideRunError<H, S, A, F, L, B, C>>;

/// The previous completed run's carried findings and the waivers it applied, read from the
/// bundle `previous` names when `carry` keeps them; none for a first review.
fn carried<H, S, A, F, L, B: BundleSource, C>(
    carry: Carry,
    previous: Option<&PriorCarry<'_>>,
    bundles: &B,
) -> Carried<H, S, A, F, L, B::Error, C> {
    let Some(previous) = previous else {
        return Ok((Vec::new(), PriorWaivers::default()));
    };
    let findings = prior_carry(carry, previous, bundles).map_err(DecideRunError::Prior)?;
    let waivers = prior_waivers(carry, previous, bundles).map_err(DecideRunError::Prior)?;
    Ok((findings, waivers))
}

/// Creates `out` (refusing one that exists by now) and writes `bundle`, when there is one, and
/// then `state` into it; the directory or file that could not be written, and why.
fn write_out(
    out: &OutDir,
    state: &DecideState,
    bundle: Option<&[u8]>,
) -> Result<(), (PathBuf, io::Error)> {
    fs::create_dir(out.path()).map_err(|source| (out.path().to_path_buf(), source))?;
    let state = state.json();
    let files = bundle
        .map(|bytes| (BUNDLE_FILE, bytes))
        .into_iter()
        .chain([(STATE_FILE, state.as_slice())]);
    for (name, bytes) in files {
        let path = out.path().join(name);
        File::create_new(&path)
            .and_then(|mut file| file.write_all(bytes).and_then(|()| file.flush()))
            .map_err(|source| (path, source))?;
    }
    Ok(())
}
