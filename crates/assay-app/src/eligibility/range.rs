//! Rule (b) of 04 §5: the PR range from git objects in Z4 (`git rev-list base_sha..head_sha`),
//! each commit's GitHub-attributed author and committer, and the clean merges from base that
//! neither (b) nor (c) counts (R3-07, R3-22).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use super::pushes::pushed;
use crate::config::CommitId;
use crate::forge::{Actor, CommitIdentity};
use crate::ledger::PushRecord;
use crate::status::UserId;

/// The most commits a PR range may hold; past it the PR is `needs-split` (R3-22).
pub const MAX_RANGE_COMMITS: usize = 250;

/// GitHub's `web-flow` user, the committer of commits made on github.com.
pub const WEB_FLOW: UserId = UserId(match std::num::NonZeroU64::new(19_864_447) {
    Some(id) => id,
    None => std::num::NonZeroU64::MIN,
});

/// The PR's git objects, as Z4 fetched them. Implemented over `assay-forge`'s object-only
/// repository by `assay-cli`.
pub trait RangeObjects {
    /// Why git could not answer.
    type Error: std::error::Error;

    /// The commits of `base..head` (`git rev-list --parents`), newest first, or
    /// [`RangeListing::TooMany`] when there are more than `max`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when git cannot list the range.
    fn range(
        &self,
        base: &CommitId,
        head: &CommitId,
        max: usize,
    ) -> Result<RangeListing, Self::Error>;

    /// The tree of `commit`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when git cannot read the commit.
    fn tree(&self, commit: &CommitId) -> Result<TreeId, Self::Error>;

    /// The tree `git merge-tree --write-tree ours theirs` writes, or
    /// [`MergeTree::Conflicted`] when the merge conflicts.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when git cannot merge for any other reason.
    fn merge_tree(&self, ours: &CommitId, theirs: &CommitId) -> Result<MergeTree, Self::Error>;

    /// Whether `ancestor` is `of` or one of its ancestors.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when git cannot answer.
    fn is_ancestor(&self, ancestor: &CommitId, of: &CommitId) -> Result<bool, Self::Error>;
}

/// What [`RangeObjects::range`] listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeListing {
    /// Every commit of the range, newest first.
    Listed(Vec<RangeCommit>),
    /// More commits than the cap.
    TooMany,
}

/// One commit of the range and its parents, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCommit {
    /// The commit.
    pub commit: CommitId,
    /// Its parents, the first parent first.
    pub parents: Vec<CommitId>,
}

/// What a clean `git merge-tree --write-tree` would record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeTree {
    /// The merge is conflict-free and writes this tree.
    Clean(TreeId),
    /// The merge conflicts.
    Conflicted,
}

/// A git tree id: 40 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreeId(String);

/// A text that is not 40 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a tree id is 40 lowercase hexadecimal digits")]
pub struct ParseTreeIdError;

impl TreeId {
    /// The id as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for TreeId {
    type Err = ParseTreeIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        // A tree id has a commit id's shape; reuse its check.
        text.parse::<CommitId>()
            .map(|_shape| Self(text.to_owned()))
            .map_err(|_shape| ParseTreeIdError)
    }
}

impl fmt::Display for TreeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who the range says touched the PR, apart from clean merges from base.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Attribution {
    /// The GitHub users who authored (git author only) or committed a counted commit.
    pub(crate) users: BTreeSet<UserId>,
    /// Whether a counted commit has an author or committer that is no GitHub user (web-flow
    /// counts as one only as committer): every owner then counts as its author.
    pub(crate) unattributed: bool,
}

/// Why a range commit's identity could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdentityMismatch;

/// A range commit a push record may exempt as a merge from base, with its two parents.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MergeCandidate<'a> {
    /// The merge commit.
    pub(crate) commit: &'a CommitId,
    /// Its first parent, the push record's `before`.
    pub(crate) ours: &'a CommitId,
    /// Its second parent, which must be an ancestor of base.
    pub(crate) theirs: &'a CommitId,
}

/// The range commits each push record may exempt: a two-parent commit `after` whose first
/// parent is the record's `before`, each commit once.
pub(crate) fn merge_candidates<'a>(
    commits: &'a [RangeCommit],
    pushes: &[PushRecord],
) -> Vec<MergeCandidate<'a>> {
    let by_id: BTreeMap<&CommitId, &RangeCommit> = commits
        .iter()
        .map(|commit| (&commit.commit, commit))
        .collect();
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for push in pushes.iter().filter_map(pushed) {
        let Some(commit) = by_id.get(push.after) else {
            continue;
        };
        if let [ours, theirs] = commit.parents.as_slice()
            && push.before == ours
            && seen.insert(&commit.commit)
        {
            candidates.push(MergeCandidate {
                commit: &commit.commit,
                ours,
                theirs,
            });
        }
    }
    candidates
}

/// The attribution of `identities` (one per range commit, in order) without the clean merges
/// from base (`clean`'s keys). Co-author trailers are ignored: only GitHub's first author, the
/// git author, counts.
pub(crate) fn attribute(
    commits: &[RangeCommit],
    identities: &[CommitIdentity],
    clean: &BTreeMap<CommitId, CommitId>,
) -> Result<Attribution, IdentityMismatch> {
    if commits.len() != identities.len() {
        return Err(IdentityMismatch);
    }
    let mut attribution = Attribution::default();
    for (commit, identity) in commits.iter().zip(identities) {
        if identity.commit != commit.commit {
            return Err(IdentityMismatch);
        }
        if clean.contains_key(&commit.commit) {
            continue;
        }
        let author = identity.authors.first().copied();
        for (actor, web_flow_allowed) in [(author, false), (Some(identity.committer), true)] {
            match actor {
                Some(Actor::User(id)) if id != WEB_FLOW || web_flow_allowed => {
                    attribution.users.insert(id);
                }
                Some(Actor::User(_) | Actor::NotAUser) | None => attribution.unattributed = true,
            }
        }
    }
    Ok(attribution)
}
