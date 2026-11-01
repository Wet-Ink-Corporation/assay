//! `assay decide-only decide` (#61 DO-11, 04 §6, J1-J10): one PR of guard's `prs` decided again
//! from the evidence bundle the ledger points at, with a fresh GitHub state snapshot, and what
//! `decide-only publish` publishes, as [`RedecideState`].
//!
//! [`run_redecide`] creates `--out` and writes the state as [`STATE_FILE`](crate::STATE_FILE) and, for a new
//! decision only, the bundle as [`DECISION_FILE`], then returns the outputs (03 §4a). In order:
//! the PR must be one of guard's `prs` and the payload guard's event (J1: the command is read
//! from the payload, never re-fetched); the live PR and its base tip's lists (D4); a command's
//! count (J7, D6), before any other write and before the evidence is read; the ledger state's
//! evidence pointer and publication register (D2); the evidence ([`read_evidence`], J4, D11);
//! the snapshot ([`redecided_state`], D3); the command recorded or rejected (D7, D10); the
//! deferral flag (D14); the inputs ([`redecision_inputs`]), and the bundle
//! ([`prepare_decision`]) unless the key is the register's (J9). Its only ledger writes are the
//! counter, the command record and the deferral flag (J6): it never takes a slot, a lease or a
//! lock, and nothing is published here.

mod command;
mod error;
mod state;
mod trigger;

use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    AGENTS_PATH, ArtifactNonce, BundleSource, Claim, CommandComments, CommandCount, CommandReplies,
    CommitId, Counted, CurrentPullRequest, DECISION_FILE, DecisionTarget, EligibilityLists,
    EvidenceBundle, EvidencePointer, EvidenceRead, EvidenceRequest, Exempt, Forge, Forgery,
    GitHubState, Ledger, LedgerSnapshot, LedgerTime, LedgerWriteError, OWNERS_PATH, PrState,
    PreparedBundle, PrerequisiteForge, PublicationRegister, PullRequestState, RedecidePorts,
    RedecideRefusal, RedecideRequest, RedecideStateError, Redecided, RedecisionRequest, Reply,
    RunProvenance, StateUpdate, Timestamp, TipListFiles, TipLists, count_command, prepare_decision,
    read_evidence, read_state, redecided_state, redecision_inputs, update_state,
};
use assay_domain::Rubric;
use assay_forge::github::{DecideOnlyEvent, ForgeError, GitHub, Sleep, Transport};

use self::command::{Commanded, settle_command};
pub use self::error::{PayloadMismatch, RedecideRunError, RedecideRunFailure};
pub use self::state::{RedecideState, RedecideStateFileError};
use self::trigger::{Trigger, trigger};
use crate::ci::{FirstAttempt, OutDir};
use crate::decide::write_out;
use crate::outputs::{DecideOnlyGuardOutputs, RedecideOutputs};

/// `state`'s publication register (D2) when decide-only may write over it for `head`: it is of
/// `head`, and was written from `state`'s own evidence or decision: by the run its evidence
/// pointer or its decision pointer names, or by a decide-only refusal of its evidence pointer
/// in repository `repo_id` (its key is [`RedecideRefusal::keyed_over`] that pointer). A
/// register of any other run (a newer pipeline run's refusal on the same head) is not
/// decide-only's to overwrite with older evidence (wi-b70b90).
fn claimable<'s>(
    state: &'s PrState,
    repo_id: NonZeroU64,
    head: &CommitId,
) -> Option<&'s PublicationRegister> {
    let register = state.published.as_ref()?;
    let evidence = state.evidence.as_ref();
    let decision = state.decision.as_ref().map(|pointer| pointer.run_id);
    let claimed = evidence.is_some_and(|pointer| pointer.run_id == register.run_id)
        || decision == Some(register.run_id)
        || evidence
            .is_some_and(|pointer| RedecideRefusal::keyed_over(repo_id, pointer, &register.key));
    (&register.head_sha == head && claimed).then_some(register)
}

/// `state`'s register and evidence pointer as decide-only's [`Claim`] for `head`, when the
/// register is [`claimable`] and the state has an evidence pointer; taken out of `state`.
pub(crate) fn claim(state: PrState, repo_id: NonZeroU64, head: &CommitId) -> Option<Claim> {
    claimable(&state, repo_id, head)?;
    Some(Claim {
        register: state.published?,
        evidence: state.evidence?,
    })
}

/// The repository's default branch, the only branch a dispatched pipeline run may run on (J4).
pub trait DefaultBranch {
    /// Why it could not be read.
    type Error: Error;

    /// The default branch's name.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the repository cannot be read exactly.
    fn default_branch(&self) -> Result<String, Self::Error>;
}

impl<T: Transport, S: Sleep> DefaultBranch for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn default_branch(&self) -> Result<String, ForgeError<T::Error>> {
        PrerequisiteForge::default_branch(self)
    }
}

/// What `decide-only decide` is given besides its ports.
#[derive(Debug)]
pub struct RedecideJob<'a> {
    /// The run, checked to be its first attempt (J2).
    pub run: &'a FirstAttempt,
    /// What guard wrote: the PRs to decide and the event behind them.
    pub guard: &'a DecideOnlyGuardOutputs,
    /// `ASSAY_PR`: the matrix entry this job decides.
    pub pr: NonZeroU64,
    /// The triggering event's payload, the only place a command is read from (J1).
    pub event: &'a DecideOnlyEvent,
    /// rubric v1, as the binary embeds it.
    pub rubric: &'a Rubric,
    /// Now: the command record's `at` and every event's `ts`.
    pub at: LedgerTime,
    /// What makes the decision artifact's name unique; stored in the outputs.
    pub nonce: ArtifactNonce,
}

/// What `decide-only decide` reads and writes through, borrowed for one call.
#[derive(Debug)]
pub struct RedecideJobPorts<'p, A, R, L> {
    /// GitHub with the `assay-verdict` App's token: the live PR, the base tip's lists, reviews,
    /// command comments and replies.
    pub app: &'p A,
    /// GitHub with the job's run token: the evidence run's provenance, its bundle and the
    /// default branch.
    pub run: &'p R,
    /// The ledger's files.
    pub ledger: &'p L,
}

/// What [`run_redecide`] returns over its ports.
pub type RedecideRunOutcome<A, R, L> = Result<Redecision, RedecideRunFailure<A, R, L>>;

/// What a re-decision left besides `--out`: its outputs, and how forged evidence is forged.
///
/// The forgery is for the job's log ([`forgery_code`]); `state.json` names only the refusal, so
/// the cause is reported rather than dropped (H-10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redecision {
    /// The outputs to append (03 §4a).
    pub outputs: RedecideOutputs,
    /// How the evidence is forged, for a [`RedecideRefusal::Forged`] refusal only.
    pub forgery: Option<Forgery>,
}

/// A step of the re-decision over the ports' error types.
type Step<T, A, R, L> = Result<T, RedecideRunFailure<A, R, L>>;

/// How a re-decision ended.
enum Ended {
    /// Nothing to publish.
    Silent,
    /// The evidence is unusable; forged evidence carries how.
    Refused(RedecideRefusal, Option<Forgery>),
    /// A new decision, as its bundle's bytes.
    Decided(PreparedBundle),
}

/// Decides `job.pr` again through `ports` and writes what `decide-only publish` publishes into
/// `out`.
///
/// It creates `out` and writes [`STATE_FILE`](crate::STATE_FILE): [`RedecideState::Silent`]
/// for an outcome with nothing to publish (a reply-only one included),
/// [`RedecideState::Refused`] for unusable evidence, or [`RedecideState::Decided`] after the
/// bundle as [`DECISION_FILE`], whose sha256 it names.
///
/// # Errors
///
/// [`RedecideRunError`] when `job.pr` is not guard's, the payload is not guard's event, a port
/// fails or the bundle or `out` cannot be built or written. Nothing is written into `out` before
/// everything it holds is ready.
pub fn run_redecide<A, R, L, E>(
    job: &RedecideJob<'_>,
    ports: &RedecideJobPorts<'_, A, R, L>,
    out: &OutDir,
) -> RedecideRunOutcome<A, R, L>
where
    A: PullRequestState<Error = E>
        + Forge<Error = E>
        + CommandComments<Error = E>
        + CommandReplies<Error = E>
        + TipLists,
    R: RunProvenance<Error = E> + BundleSource + DefaultBranch<Error = E>,
    L: Ledger<Error: 'static>,
    E: Error,
{
    if !job.guard.prs().as_slice().contains(&job.pr) {
        return Err(RedecideRunError::NotScheduled { pr: job.pr });
    }
    let ended = match trigger(job)? {
        Some(trigger) => redecide(job, ports, trigger)?,
        None => Ended::Silent,
    };
    let (state, bundle, forgery) = match ended {
        Ended::Silent => (RedecideState::Silent, None, None),
        Ended::Refused(refusal, forgery) => (RedecideState::Refused(refusal), None, forgery),
        Ended::Decided(bundle) => {
            // The state owns its copy of the digest; the bundle's bytes are written beside it.
            let digest = bundle.digest().clone();
            (RedecideState::Decided { digest }, Some(bundle), None)
        }
    };
    let bundle_file = bundle
        .as_ref()
        .map(|bundle| (DECISION_FILE, bundle.bytes()));
    write_out(out, &state.json(), bundle_file)
        .map_err(|(path, source)| RedecideRunError::Write { path, source })?;
    let outputs = match state {
        RedecideState::Decided { digest } => RedecideOutputs::bundle(job.pr, digest, &job.nonce),
        RedecideState::Silent | RedecideState::Refused(_) => RedecideOutputs::NoBundle,
    };
    Ok(Redecision { outputs, forgery })
}

/// The closed code of how evidence is forged, for the job's log: `provenance`, `too-large`,
/// `shape` or `mismatch`.
#[must_use]
pub const fn forgery_code(forgery: &Forgery) -> &'static str {
    match forgery {
        Forgery::Provenance(_) => "provenance",
        Forgery::TooLarge => "too-large",
        Forgery::Shape(_) => "shape",
        Forgery::Mismatch(_) => "mismatch",
    }
}

/// The re-decision of `job.pr` for `trigger`.
fn redecide<A, R, L, E>(
    job: &RedecideJob<'_>,
    ports: &RedecideJobPorts<'_, A, R, L>,
    trigger: Trigger<'_>,
) -> Step<Ended, A, R, L>
where
    A: PullRequestState<Error = E>
        + Forge<Error = E>
        + CommandComments<Error = E>
        + CommandReplies<Error = E>
        + TipLists,
    R: RunProvenance<Error = E> + BundleSource + DefaultBranch<Error = E>,
    L: Ledger<Error: 'static>,
    E: Error,
{
    let (pr, repo_id) = (job.pr, job.run.run_env().run_uid.repo_id);
    let silent = |reply| {
        trigger
            .answer(ports.app, pr, reply)
            .map(|()| Ended::Silent)
            .map_err(RedecideRunError::Reply)
    };
    let live = ports.app.current(pr).map_err(RedecideRunError::Live)?;
    let tip = tip_lists(ports.app.lists_at(&live.base_ref))?;
    if let Trigger::Comment { actor, .. } = trigger {
        let exempt = if tip.privileged(actor).is_some() {
            Exempt::Privileged
        } else {
            Exempt::No
        };
        let command = CommandCount {
            repo_id,
            pr,
            actor,
            exempt,
        };
        match count_command(ports.ledger, command).map_err(RedecideRunError::Count)? {
            Counted::Within { .. } => {}
            Counted::Limited { .. } => return silent(Reply::RateLimited),
        }
    }
    let state = read_state(ports.ledger, repo_id, pr).map_err(RedecideRunError::State)?;
    let register = claimable(&state, repo_id, &live.head_sha);
    let (Some(pointer), Some(register)) = (state.evidence.as_ref(), register) else {
        return silent(Reply::NoEvidence);
    };
    let evidence = match evidence(job, ports.run, pointer)? {
        EvidenceRead::Authentic(evidence) => evidence,
        EvidenceRead::Forged(forgery) => {
            return Ok(Ended::Refused(RedecideRefusal::Forged, Some(forgery)));
        }
        EvidenceRead::Absent(absence) => {
            return Ok(Ended::Refused(RedecideRefusal::Absent(absence), None));
        }
        EvidenceRead::OtherRubric => return silent(Reply::RerunNeeded),
    };
    if !unmoved(&evidence, &live) {
        return silent(Reply::HeadMoved);
    }
    let accepted = Accepted {
        job,
        trigger,
        live: &live,
        tip: &tip,
        state: &state,
        register,
        evidence: &evidence,
    };
    decide(&accepted, ports)
}

/// The evidence bundle `pointer` names, read as [`read_evidence`] reads it against the
/// repository's default branch and this build's rubric.
fn evidence<R, E, T, L>(
    job: &RedecideJob<'_>,
    run: &R,
    pointer: &EvidencePointer,
) -> Result<EvidenceRead, RedecideRunError<E, T, <R as BundleSource>::Error, L>>
where
    R: RunProvenance<Error = E> + BundleSource + DefaultBranch<Error = E>,
{
    let default_branch = run
        .default_branch()
        .map_err(RedecideRunError::DefaultBranch)?;
    let request = EvidenceRequest {
        repo_id: job.run.run_env().run_uid.repo_id,
        pointer,
        default_branch: &default_branch,
        rubric: job.rubric,
    };
    read_evidence(&request, run, run).map_err(RedecideRunError::Evidence)
}

/// Whether the PR is still at the head and base `evidence` decided.
fn unmoved(evidence: &EvidenceBundle, live: &CurrentPullRequest) -> bool {
    evidence.head() == &live.head_sha && evidence.base_ref() == live.base_ref
}

/// The base tip's list files read as lists.
fn tip_lists<G, T, B, L>(
    files: Result<TipListFiles, T>,
) -> Result<EligibilityLists, RedecideRunError<G, T, B, L>> {
    let files = files.map_err(RedecideRunError::TipLists)?;
    let owners = text(files.owners.as_deref(), OWNERS_PATH)?;
    let agents = text(files.agents.as_deref(), AGENTS_PATH)?;
    EligibilityLists::parse(owners, agents).map_err(RedecideRunError::Lists)
}

/// List file `file`'s bytes as text, `None` when the file is absent.
fn text<'b, G, T, B, L>(
    bytes: Option<&'b [u8]>,
    file: &'static str,
) -> Result<Option<&'b str>, RedecideRunError<G, T, B, L>> {
    bytes
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_not_utf8| RedecideRunError::TipListText { file })
}

/// An authentic evidence bundle of the PR's live head, and what the re-decision read before it.
struct Accepted<'a, 'j> {
    job: &'a RedecideJob<'j>,
    trigger: Trigger<'a>,
    live: &'a CurrentPullRequest,
    tip: &'a EligibilityLists,
    state: &'a PrState,
    register: &'a PublicationRegister,
    evidence: &'a EvidenceBundle,
}

/// The snapshot, the command, the deferral flag, the inputs and the bundle.
fn decide<A, R, L, E>(
    accepted: &Accepted<'_, '_>,
    ports: &RedecideJobPorts<'_, A, R, L>,
) -> Step<Ended, A, R, L>
where
    A: PullRequestState<Error = E>
        + Forge<Error = E>
        + CommandComments<Error = E>
        + CommandReplies<Error = E>
        + TipLists,
    R: BundleSource,
    L: Ledger<Error: 'static>,
    E: Error,
{
    let (job, trigger, live) = (accepted.job, accepted.trigger, accepted.live);
    let (pr, run_uid) = (job.pr, job.run.run_env().run_uid);
    let Some(github) = snapshot(accepted, ports)? else {
        return Ok(Ended::Silent);
    };
    let (github, recorded) = match trigger {
        Trigger::Comment { payload, .. } => {
            let at = job.at;
            match settle_command(ports, (run_uid, pr, at), &payload.event, &github)? {
                Commanded::Recheck => (github, None),
                Commanded::Recorded(command) => {
                    // The snapshot keeps one copy to judge; the bundle's command event reads
                    // the other.
                    (github.with_recorded(command.clone()), Some(command))
                }
                Commanded::Stop => return Ok(Ended::Silent),
            }
        }
        Trigger::Label { .. } | Trigger::Schedule => (github, None),
    };
    if !matches!(trigger, Trigger::Schedule) {
        flag_deferral(ports.ledger, run_uid.repo_id, pr, &live.head_sha)
            .map_err(RedecideRunError::Deferral)?;
    }
    let requeues = accepted
        .state
        .deferral
        .as_ref()
        .filter(|deferral| deferral.head == live.head_sha)
        .map_or(0, |deferral| deferral.count);
    let inputs = redecision_inputs(&RedecisionRequest {
        evidence: accepted.evidence,
        state: &github,
        requeues,
    })
    .map_err(RedecideRunError::Inputs)?;
    if inputs.decision_key == accepted.register.key.as_str() {
        return Ok(Ended::Silent);
    }
    let command = command::trigger_command(recorded.as_ref());
    prepare_decision(
        &DecisionTarget {
            run_uid,
            head_sha: &live.head_sha,
            base_ref: &live.base_ref,
            trigger: trigger.decision(),
            at: Timestamp::from(job.at),
        },
        &Redecided {
            evidence: accepted.evidence,
            rubric: job.rubric,
            inputs: &inputs,
            github: &github,
            command,
        },
    )
    .map(Ended::Decided)
    .map_err(RedecideRunError::Prepare)
}

/// The re-decision snapshot of the evidence's head; `None`, once the comment is answered, when
/// the PR was pushed since its evidence run (a new run decides).
fn snapshot<A, R, L, E>(
    accepted: &Accepted<'_, '_>,
    ports: &RedecideJobPorts<'_, A, R, L>,
) -> Step<Option<GitHubState>, A, R, L>
where
    A: PullRequestState<Error = E>
        + Forge<Error = E>
        + CommandComments<Error = E>
        + CommandReplies<Error = E>
        + TipLists,
    R: BundleSource,
    L: Ledger<Error: 'static>,
    E: Error,
{
    let pr = accepted.job.pr;
    let snapshot = redecided_state(
        &RedecidePorts {
            forge: ports.app,
            ledger: &LedgerSnapshot {
                ledger: ports.ledger,
                repo_id: accepted.job.run.run_env().run_uid.repo_id,
            },
            comments: ports.app,
        },
        &RedecideRequest {
            number: pr,
            recorded: accepted.evidence.recorded(),
            tip: accepted.tip,
        },
    );
    match snapshot {
        Ok(github) => Ok(Some(github)),
        Err(RedecideStateError::PushedSince) => accepted
            .trigger
            .answer(ports.app, pr, Reply::HeadMoved)
            .map(|()| None)
            .map_err(RedecideRunError::Reply),
        Err(error) => Err(RedecideRunError::Snapshot(Box::new(error))),
    }
}

/// Sets the flag of the PR's deferral record when it names `head` (D14): the requeue job then
/// runs the pipeline for it. Nothing is written when there is none, or it is set already.
fn flag_deferral<L: Ledger>(
    ledger: &L,
    repo_id: NonZeroU64,
    pr: NonZeroU64,
    head: &CommitId,
) -> Result<(), LedgerWriteError<L::Error>> {
    let flagged = |state: &PrState| {
        let deferral = state.deferral.as_ref()?;
        if &deferral.head != head || deferral.flag {
            return None;
        }
        // update_state shows a borrowed state and takes an owned next one.
        let mut next = state.clone();
        if let Some(deferral) = next.deferral.as_mut() {
            deferral.flag = true;
        }
        Some(next)
    };
    match update_state(ledger, repo_id, pr, flagged)? {
        StateUpdate::Updated(_) | StateUpdate::Unchanged(_) => Ok(()),
    }
}
