//! `decide`'s authentication of its inputs (Z4, `docs/architecture/02-architecture.md` §1, §4
//! and §5): what the rubric may believe, and when nothing may be published at all.
//!
//! Z4 believes nothing it can recompute. [`authenticate`] refuses a re-run before reading
//! anything (R3-05); re-fetches the PR's head and base ref, and stops without a check or comment
//! if either moved (R3-03, R3-06); yields to a newer run when an upstream job was cancelled;
//! authenticates every artifact it reads by the sha256 its producing job published (F18);
//! recomputes the plan from base config and the PR's objects and refuses one that differs (F15),
//! accepting only `admit`'s full-to-lite degrade (R3-27); reads every scheduled gate, a skipped
//! or failed one never passing; recomputes each gate finding's evidence and fingerprint; and
//! counts the open PRs on the head (R3-19). Anything it cannot read or reconcile is refused, not
//! modelled (wi-b5affa). Publishing is #57's; the GitHub snapshot is #58's.

mod evidence;
mod fetch;
mod gates;
mod inputs;
mod ports;
mod recompute;
mod refusal;
mod request;

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::num::NonZeroU64;

use assay_domain::{
    Authorship, Finding, GateReport, OwnerApproval, Policy, PullRequest, Readiness, Tier, Tiering,
    WeighIn,
};
use thiserror::Error;

pub(crate) use self::evidence::{Anchors, Cited, ReadFailure, Texts, read_texts};
pub use self::evidence::{MAX_EVIDENCE_BYTES, MAX_EVIDENCE_FILES, MAX_SYMBOL_BYTES};
pub use self::fetch::MAX_FETCHED_BYTES;
use self::fetch::{Artifact, Fetch, Fetcher};
pub use self::inputs::{InputsError, InputsRequest, decision_inputs};
pub use self::ports::{CurrentPullRequest, Fetched, PullOnHead, PullRequestState, RunArtifacts};
use self::recompute::AuthenticPlan;
pub use self::refusal::{Moved, OutputsProblem, PlanMismatch, PlanRefusal, Refusal, Unreadable};
pub use self::request::{
    AdmitJob, AdmitOutcome, ArtifactDocument, DecideRequest, GateJobOutputs, JobResult, PlanJob,
    Reported, ReportedGate, ReviewJob, ReviewStage, TierDegrade, UpstreamJob,
};
use crate::config::{Adapter, AssaySettings, CommitId, PluginsConfig, Sha256Digest};
use crate::contract::ContractValidator;
use crate::eligibility::{CommitRange, WaiverBindings};
use crate::plan::{ArtifactJob, EvidenceKey, PlanError, PullRequestEvent, PullRequestObjects};

/// The most documents the `review` job may publish (H-13).
pub const MAX_REVIEW_DOCUMENTS: usize = 64;

/// What `decide` may do with this run.
#[derive(Debug)]
pub enum Authentication {
    /// The PR's head or base ref moved since `plan`: emit `RunSuperseded` and write no check
    /// and no comment; the ledger record and slot release still happen (R3-06).
    Superseded(Moved),
    /// An upstream job was cancelled and a newer run exists for the head: publish nothing and
    /// leave the check `in_progress` for that run (R2-5).
    YieldToNewer,
    /// Publish `failure` with [`Refusal::reason`]: the rubric does not run.
    Refused(Refusal),
    /// The inputs are authenticated: build the decision from them.
    Authenticated(Box<AuthenticatedRun>),
}

/// What Z4 established about a run, for `decide`'s inputs (04 §5).
#[derive(Debug)]
pub struct AuthenticatedRun {
    run_key: EvidenceKey,
    /// The head the run was planned for, which GitHub still shows.
    head: CommitId,
    artifact_names: BTreeMap<ArtifactJob, String>,
    tiering: Tiering,
    tier: Tier,
    policy: Policy,
    /// What rule 5 reads of the Weigh-In gate, from base config.
    weigh_in: WeighIn,
    base_ref: String,
    open_prs_on_head: u32,
    gates: Vec<GateReport>,
    gate_findings: Vec<Finding>,
    review_documents: Vec<Vec<u8>>,
    /// The bundle's record of the run's artifact documents (#57).
    record: RunRecord,
}

/// What `decide` read and authenticated, kept for the `RunInputs` bundle (02 §5).
#[derive(Debug)]
pub(crate) struct RunRecord {
    /// The `decide` job's own artifact.
    pub(crate) bundle_artifact: String,
    /// `run.json`, then every authentic gate document, in job then file order.
    pub(crate) documents: Vec<RunDocument>,
    /// The `review` documents' names and digests, one per entry of `review_documents`.
    pub(crate) review_files: Vec<ArtifactDocument>,
}

impl RunRecord {
    /// The record of a run whose plan is `plan`, whose authentic gate documents are `gates` and
    /// whose review documents were all fetched, in the order `request` lists them.
    fn new(
        request: &DecideRequest,
        bundle_artifact: String,
        plan: RunDocument,
        gates: Vec<RunDocument>,
    ) -> Self {
        let mut documents = Vec::with_capacity(gates.len().saturating_add(1));
        documents.push(plan);
        documents.extend(gates);
        Self {
            bundle_artifact,
            documents,
            // `review_documents` fetched each of these in order, so the two line up; the
            // record owns its copy of the names and digests.
            review_files: request.review.documents.clone(),
        }
    }
}

/// An artifact document whose bytes hash to its producing job's output digest.
#[derive(Debug)]
pub(crate) struct RunDocument {
    /// The producing job.
    pub(crate) job: ArtifactJob,
    /// The file in its artifact.
    pub(crate) file: String,
    /// The digest the job published, which the bytes match.
    pub(crate) digest: Sha256Digest,
    /// The bytes as fetched.
    pub(crate) bytes: Vec<u8>,
}

/// The facts of GitHub's state snapshot a decision needs about the PR, as
/// [`GitHubState::snapshot`](crate::GitHubState::snapshot) computes them (#58).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    /// `owner_approved(head)` (04 §5).
    pub owner_approval: OwnerApproval,
    /// Whether an agent identity authored the PR.
    pub authorship: Authorship,
    /// The PR's draft flag at decide time.
    pub readiness: Readiness,
    /// How many times this head's deferred run was requeued.
    pub requeues: u32,
    /// Whether the git range fits the eligibility rule's 250 commits; past it the PR is
    /// `needs-split` whatever the plan's tier (R3-22).
    pub commits: CommitRange,
}

impl AuthenticatedRun {
    /// The evidence key Z4 recomputed (the plan's `run_key`).
    #[must_use]
    pub const fn run_key(&self) -> EvidenceKey {
        self.run_key
    }

    /// Each job's artifact name, as the authenticated plan names it.
    #[must_use]
    pub const fn artifact_names(&self) -> &BTreeMap<ArtifactJob, String> {
        &self.artifact_names
    }

    /// The tiering Z4 recomputed, before any degrade.
    #[must_use]
    pub const fn tiering(&self) -> &Tiering {
        &self.tiering
    }

    /// The tier the LLM stage ran at: the recomputed tier, or `lite` after `admit`'s accepted
    /// full-to-lite degrade (R3-27). With the classes it names the roles rule 8 requires.
    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.tier
    }

    /// Base policy (03 §1 `[assay]`).
    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Open PRs on the head into the PR's base, the PR itself included (rule 2a).
    #[must_use]
    pub const fn open_prs_on_head(&self) -> u32 {
        self.open_prs_on_head
    }

    /// One report per gate and job the plan schedules (rule 1), in job then file order.
    #[must_use]
    pub fn gates(&self) -> &[GateReport] {
        &self.gates
    }

    /// Every finding of every gate that ran, anchored by Z4: evidence replaced by the tree's
    /// line where verified, and the rule dropped where not. Validation's `gate_findings`.
    #[must_use]
    pub fn gate_findings(&self) -> &[Finding] {
        &self.gate_findings
    }

    /// The `review` job's documents, authenticated by digest and not yet read: validation's
    /// documents.
    #[must_use]
    pub fn review_documents(&self) -> &[Vec<u8>] {
        &self.review_documents
    }

    /// What a waiver of each gate fingerprint is bound to on this head (R3-13, #59): the
    /// binding digests (canonical JSON without `source.run_key`, wi-bc5341) of the authentic
    /// gate documents of every gate carrying it, for
    /// [`GitHubState::waivers_carried`](crate::GitHubState::waivers_carried).
    #[must_use]
    pub fn waiver_bindings(&self) -> WaiverBindings {
        let documents = self
            .record
            .documents
            .iter()
            .filter(|document| {
                matches!(
                    document.job,
                    ArtifactJob::GatesStatic | ArtifactJob::GatesLint | ArtifactJob::GatesTest
                )
            })
            .map(|document| (document.file.as_str(), document.bytes.as_slice()));
        WaiverBindings::of(&self.gates, documents)
    }

    /// What the bundle records of the run's artifacts.
    pub(crate) const fn record(&self) -> &RunRecord {
        &self.record
    }

    /// The PR as rubric v1 reads it: Z4's facts with the snapshot's.
    #[must_use]
    pub fn pull_request(&self, snapshot: Snapshot) -> PullRequest {
        PullRequest {
            // The decision input owns its base ref; the run keeps its own.
            base_ref: self.base_ref.clone(),
            open_prs_on_head: self.open_prs_on_head,
            tier: match snapshot.commits {
                CommitRange::Within => self.tier,
                CommitRange::NeedsSplit => Tier::NeedsSplit,
            },
            // As above: the input owns the classes.
            classes: self.tiering.classes.clone(),
            owner_approval: snapshot.owner_approval,
            authorship: snapshot.authorship,
            readiness: snapshot.readiness,
            requeues: snapshot.requeues,
        }
    }
}

/// Which call to GitHub's live state failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateStep {
    /// [`PullRequestState::current`].
    Current,
    /// [`PullRequestState::pulls_on_head`].
    PullsOnHead,
    /// [`PullRequestState::newer_run`].
    NewerRun,
}

impl StateStep {
    const fn describe(self) -> &'static str {
        match self {
            Self::Current => "re-fetch the pull request",
            Self::PullsOnHead => "list the pull requests on the head",
            Self::NewerRun => "look for a newer run",
        }
    }
}

/// Why `decide` could not reach a verdict at all. Nothing is published; the job fails.
#[derive(Debug, Error)]
pub enum DecideError<O, S, A> {
    /// The run is a re-run: privileged jobs run on attempt 1 only, and recovery is a fresh run
    /// (R3-05, T22). Nothing was read.
    #[error("refused attempt {attempt}: decide runs on attempt 1 only; start a fresh run")]
    NotFirstAttempt {
        /// The attempt.
        attempt: NonZeroU64,
    },
    /// GitHub's live state could not be read.
    #[error("could not {} on GitHub", step.describe())]
    State {
        /// The call that failed.
        step: StateStep,
        /// The port's error.
        #[source]
        source: S,
    },
    /// The artifact store could not be read.
    #[error("could not fetch {file} of the {} artifact", job.name())]
    Artifacts {
        /// The producing job.
        job: ArtifactJob,
        /// The file asked for.
        file: String,
        /// The port's error.
        #[source]
        source: A,
    },
    /// The PR's objects could not be read to recompute the plan.
    #[error("could not recompute the plan")]
    Recompute(#[source] PlanError<O, Infallible>),
    /// The PR's objects could not be read for gate evidence.
    #[error("could not read the files gate findings cite")]
    Objects(#[source] O),
    /// The objects port returned another number of blobs than asked for.
    #[error("asked for {asked} blobs, read {returned}")]
    ShortRead {
        /// Entries asked for.
        asked: usize,
        /// Blobs returned.
        returned: usize,
    },
    /// The recomputed run description could not be encoded: a defect in `decide`.
    #[error("could not encode the recomputed run description")]
    Encode(#[source] serde_json::Error),
}

impl<O, S, A> From<ReadFailure<O>> for DecideError<O, S, A> {
    fn from(failure: ReadFailure<O>) -> Self {
        match failure {
            ReadFailure::Objects(source) => Self::Objects(source),
            ReadFailure::ShortRead { asked, returned } => Self::ShortRead { asked, returned },
        }
    }
}

/// The [`DecideError`] of [`authenticate`] over `ports`' error types.
pub type DecideFailure<O, S, A> = DecideError<
    <O as PullRequestObjects>::Error,
    <S as PullRequestState>::Error,
    <A as RunArtifacts>::Error,
>;

/// The ports `decide` reads through, borrowed for one call.
#[derive(Debug)]
pub struct DecidePorts<'p, O, S, A> {
    /// The PR's objects, read at the pinned SHAs only.
    pub objects: &'p O,
    /// GitHub's live state of the PR.
    pub state: &'p S,
    /// The run's artifacts.
    pub artifacts: &'p A,
}

/// What `decide` reads documents with: the objects, the artifacts under the run's byte budget
/// and the contract validator.
struct Reader<'r, O, A> {
    objects: &'r O,
    fetcher: Fetcher<'r, A>,
    validator: &'r ContractValidator,
    /// Every authentic gate document read, valid or not, for the bundle.
    kept: Vec<RunDocument>,
}

/// [`Halt`] over `ports`' error types.
type HaltOf<O, S, A> = Halt<
    <O as PullRequestObjects>::Error,
    <S as PullRequestState>::Error,
    <A as RunArtifacts>::Error,
>;

/// Why authentication stopped early: a refusal to publish, or an error.
#[derive(Debug)]
enum Halt<O, S, A> {
    Refused(Refusal),
    Failed(DecideError<O, S, A>),
}

impl<O, S, A> From<Refusal> for Halt<O, S, A> {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl<O, S, A> From<PlanMismatch> for Halt<O, S, A> {
    fn from(mismatch: PlanMismatch) -> Self {
        Self::Refused(Refusal::PlanMismatch(mismatch))
    }
}

impl<O, S, A> From<DecideError<O, S, A>> for Halt<O, S, A> {
    fn from(error: DecideError<O, S, A>) -> Self {
        Self::Failed(error)
    }
}

/// Authenticates one pipeline run's inputs for `decide`.
///
/// `request`'s job outputs are read against the PR's objects, GitHub's live state and the run's
/// artifacts through `ports`, with every document checked by `validator`.
///
/// # Examples
///
/// ```
/// use assay_app::{
///     Authentication, ContractValidator, DecideFailure, DecidePorts, DecideRequest,
///     PullRequestObjects, PullRequestState, RunArtifacts, authenticate,
/// };
///
/// # fn demo<O: PullRequestObjects, S: PullRequestState, A: RunArtifacts>(
/// #     request: &DecideRequest,
/// #     ports: &DecidePorts<'_, O, S, A>,
/// #     validator: &ContractValidator,
/// # ) -> Result<(), DecideFailure<O, S, A>> {
/// match authenticate(request, ports, validator)? {
///     // Write nothing: no check, no comment.
///     Authentication::Superseded(_) | Authentication::YieldToNewer => {}
///     // Publish `failure` with this reason.
///     Authentication::Refused(refusal) => drop(refusal.reason()),
///     // Build the decision inputs from the run.
///     Authentication::Authenticated(run) => drop(run.gates()),
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// [`DecideError::NotFirstAttempt`] for a re-run, before any port is called; otherwise a port
/// failed ([`DecideError::State`], [`DecideError::Artifacts`], [`DecideError::Recompute`],
/// [`DecideError::Objects`], [`DecideError::ShortRead`]) or the recomputed plan could not be
/// encoded ([`DecideError::Encode`]). A refusal is an [`Authentication`], not an error.
pub fn authenticate<O: PullRequestObjects, S: PullRequestState, A: RunArtifacts>(
    request: &DecideRequest,
    ports: &DecidePorts<'_, O, S, A>,
    validator: &ContractValidator,
) -> Result<Authentication, DecideFailure<O, S, A>> {
    let attempt = request.plan.run_uid.run_attempt;
    if attempt.get() != 1 {
        return Err(DecideError::NotFirstAttempt { attempt });
    }
    match run(request, ports, validator) {
        Ok(authentication) => Ok(authentication),
        Err(Halt::Refused(refusal)) => Ok(Authentication::Refused(refusal)),
        Err(Halt::Failed(error)) => Err(error),
    }
}

/// The checks of [`authenticate`] after the attempt guard, in the order of 02 §1.
fn run<O: PullRequestObjects, S: PullRequestState, A: RunArtifacts>(
    request: &DecideRequest,
    ports: &DecidePorts<'_, O, S, A>,
    validator: &ContractValidator,
) -> Result<Authentication, HaltOf<O, S, A>> {
    let event = &request.plan.pull_request;
    let state = ports.state;
    let current = state
        .current(event.number)
        .map_err(state_error(StateStep::Current))?;
    if let Some(moved) = moved(event, &current) {
        return Ok(Authentication::Superseded(moved));
    }
    if let Some(job) = request.cancelled() {
        let newer = state
            .newer_run(request.plan.run_uid, &event.head_sha)
            .map_err(state_error(StateStep::NewerRun))?;
        return if newer {
            Ok(Authentication::YieldToNewer)
        } else {
            Err(Refusal::Cancelled { job }.into())
        };
    }
    if request.plan_job.result != JobResult::Success {
        let result = request.plan_job.result;
        return Err(Refusal::PlanNotSucceeded { result }.into());
    }
    let mut reader = Reader {
        objects: ports.objects,
        fetcher: Fetcher::new(ports.artifacts),
        validator,
        kept: Vec::new(),
    };
    let plan = recompute::authenticate_plan(request, &mut reader)?;
    let pulls = state
        .pulls_on_head(&event.head_sha)
        .map_err(state_error(StateStep::PullsOnHead))?;
    let gates = gates::gates(request, &plan, &mut reader)?;
    let review_documents = review_documents(request, &plan, &mut reader.fetcher)?;
    let AuthenticPlan {
        recomputed,
        artifact_names,
        bundle_artifact,
        document,
        tier,
    } = plan;
    let record = RunRecord::new(request, bundle_artifact, document, reader.kept);
    Ok(Authentication::Authenticated(Box::new(AuthenticatedRun {
        run_key: recomputed.run_key,
        // The run owns its head; the request keeps its own.
        head: event.head_sha.clone(),
        artifact_names,
        policy: policy(recomputed.config.plugins.assay()),
        weigh_in: weigh_in(&recomputed.config.plugins),
        tiering: recomputed.tiering,
        tier,
        // The run owns its base ref; the request keeps its own.
        base_ref: event.base_ref.clone(),
        open_prs_on_head: open_prs_on_head(&pulls, event),
        gates: gates.reports,
        gate_findings: gates.findings,
        review_documents,
        record,
    })))
}

/// Wraps a state port error from `step`, for `map_err`.
const fn state_error<O, S, A>(step: StateStep) -> impl FnOnce(S) -> Halt<O, S, A> {
    move |source| Halt::Failed(DecideError::State { step, source })
}

/// Which of the PR's refs moved from what the run was planned for.
fn moved(event: &PullRequestEvent, current: &CurrentPullRequest) -> Option<Moved> {
    match (
        current.head_sha != event.head_sha,
        current.base_ref != event.base_ref,
    ) {
        (false, false) => None,
        (true, false) => Some(Moved::Head),
        (false, true) => Some(Moved::BaseRef),
        (true, true) => Some(Moved::HeadAndBaseRef),
    }
}

/// Open PRs on the head into the PR's base: the PR itself, even if the listing lags, plus every
/// other open one, each number counted once.
fn open_prs_on_head(pulls: &[PullOnHead], event: &PullRequestEvent) -> u32 {
    let others: BTreeSet<NonZeroU64> = pulls
        .iter()
        .filter(|pull| pull.open && pull.base_ref == event.base_ref && pull.number != event.number)
        .map(|pull| pull.number)
        .collect();
    // More PRs than `u32::MAX` are still more than one.
    u32::try_from(others.len())
        .unwrap_or(u32::MAX)
        .saturating_add(1)
}

/// The `review` job's documents, each authenticated by the digest the job published.
fn review_documents<O, S, A: RunArtifacts>(
    request: &DecideRequest,
    plan: &AuthenticPlan,
    fetcher: &mut Fetcher<'_, A>,
) -> Result<Vec<Vec<u8>>, Halt<O, S, A::Error>> {
    let documents = &request.review.documents;
    let refuse = |problem| Refusal::Outputs {
        job: UpstreamJob::Review,
        problem,
    };
    if request.review.result == JobResult::Skipped && !documents.is_empty() {
        return Err(refuse(OutputsProblem::SkippedJobReported).into());
    }
    if documents.len() > MAX_REVIEW_DOCUMENTS {
        return Err(refuse(OutputsProblem::TooManyDocuments).into());
    }
    let mut files = BTreeSet::new();
    for document in documents {
        if !is_document_name(&document.file) {
            return Err(refuse(OutputsProblem::FileName).into());
        }
        if !files.insert(document.file.as_str()) {
            return Err(refuse(OutputsProblem::DuplicateFile).into());
        }
    }
    let job = ArtifactJob::Review;
    let Some(name) = plan.artifact_names.get(&job) else {
        return Err(PlanMismatch::ArtifactNames.into());
    };
    let artifact = Artifact { job, name };
    documents
        .iter()
        .map(|document| {
            let file = &document.file;
            let unreadable = |why| Refusal::ArtifactUnreadable {
                job,
                // The refusal names the file it could not read.
                file: file.clone(),
                why,
            };
            match fetcher.fetch(artifact, file, &document.digest)? {
                Fetch::Authentic(bytes) => Ok(bytes),
                Fetch::Missing => Err(unreadable(Unreadable::Missing).into()),
                Fetch::TooLarge => Err(unreadable(Unreadable::TooLarge).into()),
                Fetch::Tampered => Err(Refusal::ArtifactDigest {
                    job,
                    file: file.clone(),
                }
                .into()),
            }
        })
        .collect()
}

/// Whether `file` is a lowercase name of letters, digits and `-` (1 to 64 bytes, starting with
/// a letter or digit) followed by `.json`: never a path.
fn is_document_name(file: &str) -> bool {
    file.strip_suffix(".json").is_some_and(|stem| {
        stem.len() <= 64
            && stem
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

/// What rule 5 reads of the Weigh-In gate. Its adapter runs non-required only and never
/// reports a standing (04 §9, #109), so a base config that requires it holds no evidence for the
/// change; otherwise the rule never fires.
pub(crate) fn weigh_in(plugins: &PluginsConfig) -> WeighIn {
    let required = plugins
        .plugins()
        .iter()
        .any(|plugin| plugin.required() && matches!(plugin.adapter(), Adapter::WeighIn { .. }));
    if required {
        WeighIn::NoEvidence
    } else {
        WeighIn::NotRequired
    }
}

/// Base policy as the rubric reads it.
pub(crate) fn policy(settings: &AssaySettings) -> Policy {
    Policy {
        // The policy owns its lists; the configuration keeps its own.
        protected_bases: settings.protected_bases.clone(),
        llm_blockers: settings.llm_blockers,
        llm_majors: settings.llm_majors,
        on_infra_failure: settings.on_infra_failure,
        block_on_infra_for: settings.block_on_infra_for.clone(),
    }
}
