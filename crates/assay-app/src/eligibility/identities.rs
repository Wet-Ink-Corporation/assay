//! Reading clause (b) of 04 §5 from a PR's range: one reading for decide's snapshot
//! ([`github_state`](super::github_state)) and for admit's dismissals ([`range_identities`],
//! #60 ST-11), so the two cannot attribute a range differently.

use std::collections::BTreeMap;

use thiserror::Error;

use super::SnapshotStep;
use super::range::{
    MAX_RANGE_COMMITS, MergeCandidate, MergeTree, RangeListing, RangeObjects, attribute,
    merge_candidates,
};
use super::rule::RangeFacts;
use crate::admit::RangeIdentities;
use crate::config::CommitId;
use crate::forge::Forge;
use crate::ledger::PushRecord;

/// The range a clause (b) reading is of.
#[derive(Debug, Clone, Copy)]
pub struct RangeRequest<'a> {
    /// The base commit the run is pinned to.
    pub base_sha: &'a CommitId,
    /// The head commit the run is pinned to.
    pub head_sha: &'a CommitId,
    /// The PR's push records: a clean merge from base is exempt only when a push record pushed
    /// it onto its first parent.
    pub pushes: &'a [PushRecord],
}

/// Why the range could not be read.
#[derive(Debug, Error)]
pub enum RangeError<G, F> {
    /// A git call failed.
    #[error("could not {step} with git")]
    Range {
        /// The call: [`SnapshotStep::RangeList`] or [`SnapshotStep::MergeCheck`].
        step: SnapshotStep,
        /// git's error.
        #[source]
        source: G,
    },
    /// GitHub's commit identities could not be read.
    #[error("could not read the commit identities from GitHub")]
    Forge(#[source] F),
    /// GitHub's commit identities do not answer the range commit for commit, in order.
    #[error("GitHub's commit identities do not match the PR range")]
    IdentityMismatch,
}

/// What [`read_range`] found.
pub(super) struct ReadRange {
    /// What the range says about (b).
    pub(super) facts: RangeFacts,
    /// The clean merges from base of the range, each with its first parent.
    pub(super) merges: BTreeMap<CommitId, CommitId>,
}

/// The authors and committers of `request`'s range for clause (b).
///
/// They are read from `objects` and `forge` exactly as [`github_state`](super::github_state)
/// reads them: GitHub's first author
/// and the committer of each commit (web-flow only as committer), clean merges from base that a
/// push record pushed left out. A range past [`MAX_RANGE_COMMITS`] cannot be attributed, so it
/// leaves nobody eligible.
///
/// # Errors
///
/// [`RangeError`] when git or GitHub cannot be read, or GitHub's identities do not answer the
/// range.
pub fn range_identities<G: RangeObjects, F: Forge>(
    objects: &G,
    forge: &F,
    request: &RangeRequest<'_>,
) -> Result<RangeIdentities, RangeError<G::Error, F::Error>> {
    let ReadRange { facts, merges } = read_range(objects, forge, request)?;
    let clean = merges.into_keys().collect();
    Ok(match facts {
        RangeFacts::Attributed {
            users,
            unattributed,
        } => RangeIdentities::attributed(users, unattributed, clean),
        RangeFacts::TooLarge => RangeIdentities::unattributable(),
    })
}

/// Lists, attributes and classifies `request`'s range.
pub(super) fn read_range<G: RangeObjects, F: Forge>(
    objects: &G,
    forge: &F,
    request: &RangeRequest<'_>,
) -> Result<ReadRange, RangeError<G::Error, F::Error>> {
    let git = |step| move |source| RangeError::Range { step, source };
    let listing = objects
        .range(request.base_sha, request.head_sha, MAX_RANGE_COMMITS)
        .map_err(git(SnapshotStep::RangeList))?;
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
    for candidate in merge_candidates(&commits, request.pushes) {
        if is_clean_merge(objects, request.base_sha, candidate)
            .map_err(git(SnapshotStep::MergeCheck))?
        {
            // The map outlives `commits`' borrow: it keys the push records' exemption.
            merges.insert(candidate.commit.clone(), candidate.ours.clone());
        }
    }
    // The port takes the ids alone; at most 250 of them.
    let ids: Vec<CommitId> = commits.iter().map(|commit| commit.commit.clone()).collect();
    let identities = forge.commit_identities(&ids).map_err(RangeError::Forge)?;
    let attribution = attribute(&commits, &identities, &merges)
        .map_err(|_mismatch| RangeError::IdentityMismatch)?;
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
