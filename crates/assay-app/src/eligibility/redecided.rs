//! decide-only's GitHub state snapshot (D3 A, R3-26, T21): the evidence run's recorded range
//! and push facts, the lists narrowed by the protected base's tip, and fresh push records,
//! reviews and command records judged by the one rule. No git object is read.

use std::num::NonZeroU64;

use thiserror::Error;

use super::approvals::approvals;
use super::error::SnapshotStep;
use super::ports::{CommandComments, SnapshotLedger};
use super::pushes::{MAX_PUSH_RECORDS, push_clock};
use super::recorded::RecordedState;
use super::rule::Rule;
use super::state::GitHubState;
use super::timestamp::Timestamp;
use super::{CommandSources, JudgeError, MAX_COMMAND_RECORDS, judge_commands};
use crate::config::EligibilityLists;
use crate::forge::Forge;
use crate::ledger::PushRecord;
use crate::publish::CommentId;

/// The ports the re-decision snapshot reads, borrowed for one call.
#[derive(Debug)]
pub struct RedecidePorts<'p, F, L, C> {
    /// The PR's reviews.
    pub forge: &'p F,
    /// Push and command records.
    pub ledger: &'p L,
    /// The comments behind command records.
    pub comments: &'p C,
}

/// What the re-decision snapshot is taken of.
#[derive(Debug, Clone, Copy)]
pub struct RedecideRequest<'a> {
    /// The PR.
    pub number: NonZeroU64,
    /// What the authenticated evidence bundle recorded of its head
    /// ([`EvidenceBundle::recorded`](crate::EvidenceBundle::recorded)).
    pub recorded: &'a RecordedState,
    /// The eligibility lists at the protected base's tip, which narrow the recorded ones.
    pub tip: &'a EligibilityLists,
}

/// Why no re-decision snapshot was taken. decide-only then decides nothing.
#[derive(Debug, Error)]
pub enum RedecideStateError<F, L, C> {
    /// A push record moved the head's push clock, or pushed another head, since the evidence
    /// run: the recorded range and pushers no longer describe the PR, so a new run decides.
    #[error("the PR was pushed since its evidence run")]
    PushedSince,
    /// The PR's reviews could not be listed.
    #[error("could not list the reviews from GitHub")]
    Reviews(#[source] F),
    /// A ledger call failed.
    #[error("could not {step} from the ledger")]
    Ledger {
        /// The call.
        step: SnapshotStep,
        /// The ledger's error.
        #[source]
        source: L,
    },
    /// A command's comment could not be re-fetched.
    #[error("could not re-fetch a command's comment")]
    Comments(#[source] C),
    /// The ledger holds more than [`MAX_PUSH_RECORDS`] push records for the PR.
    #[error("the PR has more than {MAX_PUSH_RECORDS} push records")]
    TooManyPushRecords,
    /// The ledger holds more than [`MAX_COMMAND_RECORDS`] command records for the PR.
    #[error("the PR has more than {MAX_COMMAND_RECORDS} command records")]
    TooManyCommandRecords,
    /// Two command records name one comment.
    #[error("two command records name comment {}", comment.0)]
    RepeatedCommand {
        /// The comment.
        comment: CommentId,
    },
}

/// The error of [`redecided_state`] for these ports.
type PortsError<F, L, C> = RedecideStateError<
    <F as Forge>::Error,
    <L as SnapshotLedger>::Error,
    <C as CommandComments>::Error,
>;

/// Takes decide-only's snapshot of the head `request.recorded` was recorded of.
///
/// The rule is the recorded one (author, range, pushers) with the recorded lists
/// [narrowed](EligibilityLists::narrowed_to) by `request.tip`, so a removal at the tip takes
/// effect at once and an addition waits for a new run (R3-26). The push records are re-read
/// first: when the head's push clock is not the recorded one, or a record is later than it
/// (another head was pushed), the snapshot is [`RedecideStateError::PushedSince`] and nothing
/// else is read. Reviews and command records are read fresh and judged as `github_state`
/// judges them.
///
/// Calls: one ledger read of each kind (capped), one review listing and one comment fetch per
/// command record.
///
/// # Errors
///
/// [`RedecideStateError`] when the PR was pushed since, a port fails, a ledger list is over its
/// cap, or two command records name one comment.
pub fn redecided_state<F, L, C>(
    ports: &RedecidePorts<'_, F, L, C>,
    request: &RedecideRequest<'_>,
) -> Result<GitHubState, PortsError<F, L, C>>
where
    F: Forge,
    L: SnapshotLedger,
    C: CommandComments,
{
    let recorded = request.recorded;
    let pushes = ports
        .ledger
        .push_records(request.number, MAX_PUSH_RECORDS)
        .map_err(|source| RedecideStateError::Ledger {
            step: SnapshotStep::PushRecords,
            source,
        })?
        .within(MAX_PUSH_RECORDS)
        .ok_or(RedecideStateError::TooManyPushRecords)?;
    let clock = recorded.push_clock();
    if pushed_since(&pushes, recorded, clock) {
        return Err(RedecideStateError::PushedSince);
    }
    let rule = recorded.rule(request.tip);
    let head = recorded.head();
    let reviews = ports
        .forge
        .reviews(request.number)
        .map_err(RedecideStateError::Reviews)?;
    let approvals = approvals(&reviews, head, clock, &rule);
    let sources = CommandSources {
        number: request.number,
        ledger: ports.ledger,
        comments: ports.comments,
    };
    let commands =
        judge_commands(&sources, head, clock, &rule).map_err(RedecideStateError::of_commands)?;
    Ok(GitHubState {
        // The snapshot owns its head; the bundle keeps its own.
        head: head.clone(),
        commits: recorded.commits(),
        rule,
        clock,
        pushes,
        approvals,
        commands,
    })
}

/// Whether `pushes` say the PR moved since `recorded`: the head's clock is not `clock`, or a
/// record is later than it.
fn pushed_since(pushes: &[PushRecord], recorded: &RecordedState, clock: Option<Timestamp>) -> bool {
    push_clock(pushes, recorded.head()) != clock
        || clock.is_some_and(|clock| {
            pushes
                .iter()
                .any(|record| Timestamp::from(record.push.at) > clock)
        })
}

impl<F, L, C> RedecideStateError<F, L, C> {
    /// The command judgement's failure as the re-decision snapshot's.
    fn of_commands(error: JudgeError<L, C>) -> Self {
        match error {
            JudgeError::Ledger(source) => Self::Ledger {
                step: SnapshotStep::CommandRecords,
                source,
            },
            JudgeError::TooMany => Self::TooManyCommandRecords,
            JudgeError::Repeated(comment) => Self::RepeatedCommand { comment },
            JudgeError::Comments(source) => Self::Comments(source),
        }
    }
}

impl RecordedState {
    /// The rule a re-decision judges by: the recorded author, range and pushers, with the
    /// recorded lists narrowed by `tip`.
    fn rule(&self, tip: &EligibilityLists) -> Rule {
        Rule {
            lists: self.lists().narrowed_to(tip),
            author: self.author(),
            // The rule owns its range; the bundle keeps the recorded one.
            range: self.range.clone(),
            // As above: the rule owns its pushers.
            pushers: self.pushers().clone(),
        }
    }
}
