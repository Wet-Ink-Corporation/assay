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
mod grammar;
mod ports;
mod pushes;
mod range;
mod record;
mod rule;
mod state;
mod timestamp;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use thiserror::Error;

pub use self::approvals::IgnoredApproval;
pub use self::commands::{
    CommandRecord, CommandRejected, CommentAction, CommentEvent, IssueComment, MAX_COMMAND_RECORDS,
    Recorded, VoidCause,
};
use self::commands::{JudgedCommand, applicability, void_cause};
pub use self::grammar::{AckedSet, Command, MAX_COMMAND_REASON_CHARS, Parsed, parse_command};
pub use self::ports::{CommandComments, SnapshotLedger};
use self::pushes::push_clock;
pub use self::pushes::{MAX_PUSH_RECORDS, PushKind, PushRecord, PushTrigger, push_record};
pub use self::range::{
    MAX_RANGE_COMMITS, MergeTree, ParseTreeIdError, RangeCommit, RangeListing, RangeObjects,
    TreeId, WEB_FLOW,
};
use self::range::{MergeCandidate, attribute, merge_candidates};
pub(crate) use self::record::StateRecord;
pub use self::rule::Ineligible;
use self::rule::{RangeFacts, Rule};
pub use self::state::{CommitRange, GitHubState};
pub use self::timestamp::{InvalidTimestamp, Timestamp};
use crate::config::{CommitId, EligibilityLists};
use crate::forge::Forge;
use crate::publish::CommentId;
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

/// Why no snapshot could be taken. `decide` then fails closed.
#[derive(Debug, Error)]
pub enum StateError<F, G, L, C> {
    /// GitHub's reviews or commit identities could not be read.
    #[error("could not read reviews or commit identities from GitHub")]
    Forge(#[source] F),
    /// git could not list or merge the range.
    #[error("could not read the PR range from git")]
    Range(#[source] G),
    /// The ledger could not be read.
    #[error("could not read the PR's ledger records")]
    Ledger(#[source] L),
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
    /// GitHub's commit identities do not answer the range commit for commit, in order.
    #[error("GitHub's commit identities do not match the PR range")]
    IdentityMismatch,
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
        .push_records(request.number)
        .map_err(StateError::Ledger)?;
    if pushes.len() > MAX_PUSH_RECORDS {
        return Err(StateError::TooManyPushRecords);
    }
    let ReadRange { facts, merges } = read_range(ports, request, &pushes)?;
    let commits = match facts {
        RangeFacts::TooLarge => CommitRange::NeedsSplit,
        RangeFacts::Attributed { .. } => CommitRange::Within,
    };
    let senders = pushes
        .iter()
        .filter(|record| record.kind == PushKind::Push && !is_merge_push(&merges, record))
        .map(|record| record.sender)
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
        .map_err(StateError::Forge)?;
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

/// What [`read_range`] found.
struct ReadRange {
    /// What the range says about (b).
    facts: RangeFacts,
    /// The clean merges from base of the range, each with its first parent.
    merges: BTreeMap<CommitId, CommitId>,
}

/// Lists, attributes and classifies the range.
fn read_range<F, G, L, C>(
    ports: &StatePorts<'_, F, G, L, C>,
    request: &StateRequest<'_>,
    pushes: &[PushRecord],
) -> Result<ReadRange, PortsError<F, G, L, C>>
where
    F: Forge,
    G: RangeObjects,
    L: SnapshotLedger,
    C: CommandComments,
{
    let listing = ports
        .objects
        .range(request.base_sha, request.head_sha, MAX_RANGE_COMMITS)
        .map_err(StateError::Range)?;
    let commits = match listing {
        RangeListing::Listed(commits) if commits.len() <= MAX_RANGE_COMMITS => commits,
        RangeListing::Listed(_) | RangeListing::TooMany => {
            return Ok(ReadRange {
                facts: RangeFacts::TooLarge,
                merges: BTreeMap::new(),
            });
        }
    };
    let mut merges = BTreeMap::new();
    for candidate in merge_candidates(&commits, pushes) {
        if is_clean_merge(ports.objects, request.base_sha, candidate).map_err(StateError::Range)? {
            // The map outlives `commits`' borrow: it keys the push records' exemption.
            merges.insert(candidate.commit.clone(), candidate.ours.clone());
        }
    }
    // The port takes the ids alone; at most 250 of them.
    let ids: Vec<CommitId> = commits.iter().map(|commit| commit.commit.clone()).collect();
    let identities = ports
        .forge
        .commit_identities(&ids)
        .map_err(StateError::Forge)?;
    let attribution = attribute(&commits, &identities, &merges)
        .map_err(|_mismatch| StateError::IdentityMismatch)?;
    Ok(ReadRange {
        facts: RangeFacts::Attributed {
            users: attribution.users,
            unattributed: attribution.unattributed,
        },
        merges,
    })
}

/// Whether `candidate` is a clean merge from base: its second parent is an ancestor of `base`
/// and its tree is the conflict-free `git merge-tree --write-tree` of its parents.
fn is_clean_merge<G: RangeObjects>(
    objects: &G,
    base: &CommitId,
    candidate: MergeCandidate<'_>,
) -> Result<bool, G::Error> {
    if !objects.is_ancestor(candidate.theirs, base)? {
        return Ok(false);
    }
    match objects.merge_tree(candidate.ours, candidate.theirs)? {
        MergeTree::Clean(merged) => Ok(objects.tree(candidate.commit)? == merged),
        MergeTree::Conflicted => Ok(false),
    }
}

/// Whether `record` pushed a clean merge from base onto its first parent.
fn is_merge_push(merges: &BTreeMap<CommitId, CommitId>, record: &PushRecord) -> bool {
    record
        .before
        .as_ref()
        .is_some_and(|before| merges.get(&record.after) == Some(before))
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
        .command_records(request.number)
        .map_err(StateError::Ledger)?;
    if records.len() > MAX_COMMAND_RECORDS {
        return Err(StateError::TooManyCommandRecords);
    }
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
