//! The eligibility rule of every privileged action and the GitHub state snapshot `decide` reads
//! (`docs/architecture/04-pipeline.md` §5, P10, T9, T21, ADR-16).
//!
//! An id is eligible when it is in base `.assay/owners`' `owners` or `approvers`, in neither
//! `maintainers` nor `.assay/agents`, and is not (a) the PR author, (b) a GitHub-attributed git
//! author or committer of a commit of `base_sha..head_sha`, nor (c) the sender of a push record
//! for the PR; clean merges from base count for neither (b) nor (c). A commit no GitHub user is
//! attributed (web-flow as committer apart) counts as authored by every owner. Approvals,
//! commands and pushes are read from Z4's own sources, never from text read later: the git range
//! from objects, pushes and commands from the ledger, and each command's comment re-fetched only
//! to revoke it (wi-b5affa: anything ambiguous makes an id ineligible or voids a command).

mod approvals;
mod commands;
mod counter;
mod error;
mod grammar;
mod identities;
mod ledger;
mod ports;
mod pushes;
mod range;
mod record;
mod rule;
mod state;
mod timestamp;
mod waive;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

pub use self::approvals::IgnoredApproval;
pub(crate) use self::commands::recorded_command;
pub use self::commands::{
    CommandRejected, CommandTimeError, CommentAction, CommentEvent, IssueComment,
    MAX_COMMAND_RECORDS, Recorded, SnapshotCommand, UnreadCommand, VoidCause,
};
use self::commands::{JudgedCommand, applicability, void_cause};
pub use self::counter::{CommandCount, Counted, Exempt, MAX_COMMANDS_PER_ACTOR, count_command};
pub use self::error::{SnapshotStep, StateError};
pub use self::grammar::{AckedSet, Command, MAX_COMMAND_REASON_CHARS, Parsed, parse_command};
pub use self::identities::{RangeError, RangeRequest, range_identities};
use self::identities::{ReadRange, read_range};
pub use self::ledger::{LedgerSnapshot, LedgerSnapshotError};
pub use self::ports::{CommandComments, LedgerRecords, MAX_COMMENT_BYTES, SnapshotLedger};
pub use self::pushes::MAX_PUSH_RECORDS;
use self::pushes::{push_clock, pushed};
pub use self::range::{
    MAX_RANGE_COMMITS, MergeTree, ParseTreeIdError, RangeCommit, RangeListing, RangeObjects,
    TreeId, WEB_FLOW,
};
pub(crate) use self::record::{StateRecord, StateRecordIn};
pub use self::rule::Ineligible;
use self::rule::{RangeFacts, Rule};
pub use self::state::{CommitRange, GitHubState};
pub use self::timestamp::{InvalidTimestamp, Timestamp};
pub(crate) use self::waive::canonical_json;
pub use self::waive::{PriorWaivers, WaiverBindings, waiver_events};
use crate::config::{CommitId, EligibilityLists};
use crate::forge::Forge;
use crate::status::UserId;

/// What the snapshot is taken of.
#[derive(Debug, Clone, Copy)]
pub struct StateRequest<'a> {
    /// The PR.
    pub number: NonZeroU64,
    /// Its author (`pull_request.user.id`).
    pub author: UserId,
    /// The base commit the run is pinned to.
    pub base_sha: &'a CommitId,
    /// The head commit the run is pinned to.
    pub head_sha: &'a CommitId,
    /// The eligibility lists: base `.assay/owners` and `.assay/agents` at `base_sha`, or for
    /// decide-only those [narrowed](EligibilityLists::narrowed_to) by the protected base's tip.
    pub lists: &'a EligibilityLists,
}

/// The ports the snapshot reads, borrowed for one call.
#[derive(Debug)]
pub struct StatePorts<'p, F, G, L, C> {
    /// Reviews and commit identities.
    pub forge: &'p F,
    /// The PR's git objects.
    pub objects: &'p G,
    /// Push and command records.
    pub ledger: &'p L,
    /// The comments behind command records.
    pub comments: &'p C,
}

/// The error of [`github_state`] for these ports.
type PortsError<F, G, L, C> = StateError<
    <F as Forge>::Error,
    <G as RangeObjects>::Error,
    <L as SnapshotLedger>::Error,
    <C as CommandComments>::Error,
>;

/// Takes the GitHub state snapshot of `request`'s PR head: the eligibility facts, the standing
/// approvals and the command records, each judged by the one rule.
///
/// Calls: one `rev-list` (at most [`MAX_RANGE_COMMITS`] commits, else `needs-split` and nobody
/// eligible), three git calls per merge candidate, one identity lookup over the range, one
/// review listing, one ledger read of each kind (capped) and one comment fetch per command
/// record.
///
/// # Errors
///
/// [`StateError`] when a port fails, a ledger list is over its cap, two command records name one
/// comment, or GitHub's identities do not match the range.
pub fn github_state<F, G, L, C>(
    ports: &StatePorts<'_, F, G, L, C>,
    request: &StateRequest<'_>,
) -> Result<GitHubState, PortsError<F, G, L, C>>
where
    F: Forge,
    G: RangeObjects,
    L: SnapshotLedger,
    C: CommandComments,
{
    let pushes = ports
        .ledger
        .push_records(request.number, MAX_PUSH_RECORDS)
        .map_err(StateError::ledger(SnapshotStep::PushRecords))?
        .within(MAX_PUSH_RECORDS)
        .ok_or(StateError::TooManyPushRecords)?;
    let range = RangeRequest {
        base_sha: request.base_sha,
        head_sha: request.head_sha,
        pushes: &pushes,
    };
    let ReadRange { facts, merges } =
        read_range(ports.objects, ports.forge, &range).map_err(StateError::of_range)?;
    let commits = match facts {
        RangeFacts::TooLarge => CommitRange::NeedsSplit,
        RangeFacts::Attributed { .. } => CommitRange::Within,
    };
    let senders = pushes
        .iter()
        .filter_map(pushed)
        .filter(|push| !is_merge_push(&merges, push))
        .map(|push| push.sender)
        .collect();
    let rule = Rule {
        // The snapshot keeps the lists it judged by, for the bundle (R3-02).
        lists: request.lists.clone(),
        author: request.author,
        range: facts,
        pushers: senders,
    };
    let clock = push_clock(&pushes, request.head_sha);
    let reviews = ports
        .forge
        .reviews(request.number)
        .map_err(StateError::forge(SnapshotStep::Reviews))?;
    let approvals = approvals::approvals(&reviews, request.head_sha, clock, &rule);
    let commands = judge_commands(ports, request, clock, &rule)?;
    Ok(GitHubState {
        head: request.head_sha.clone(),
        rule,
        commits,
        clock,
        pushes,
        approvals,
        commands,
    })
}

/// Whether `push` pushed a clean merge from base onto its first parent.
fn is_merge_push(merges: &BTreeMap<CommitId, CommitId>, push: &pushes::Pushed<'_>) -> bool {
    merges.get(push.after) == Some(push.before)
}

/// Every command record of the PR, judged: void when its comment no longer stands, else as it
/// applies to this head.
fn judge_commands<F, G, L, C>(
    ports: &StatePorts<'_, F, G, L, C>,
    request: &StateRequest<'_>,
    clock: Option<Timestamp>,
    rule: &Rule,
) -> Result<Vec<JudgedCommand>, PortsError<F, G, L, C>>
where
    F: Forge,
    G: RangeObjects,
    L: SnapshotLedger,
    C: CommandComments,
{
    let records = ports
        .ledger
        .command_records(request.number, MAX_COMMAND_RECORDS)
        .map_err(StateError::ledger(SnapshotStep::CommandRecords))?
        .within(MAX_COMMAND_RECORDS)
        .ok_or(StateError::TooManyCommandRecords)?;
    let mut seen = BTreeSet::new();
    let mut judged = Vec::with_capacity(records.len());
    for record in records {
        if !seen.insert(record.comment_id) {
            return Err(StateError::RepeatedCommand {
                comment: record.comment_id,
            });
        }
        let comment = ports
            .comments
            .comment(record.comment_id)
            .map_err(StateError::Comments)?;
        let status = match void_cause(&record, comment.as_ref()) {
            Some(cause) => commands::CommandStatus::Void(cause),
            None => applicability(&record, request.head_sha, clock, rule),
        };
        judged.push(JudgedCommand { record, status });
    }
    Ok(judged)
}
