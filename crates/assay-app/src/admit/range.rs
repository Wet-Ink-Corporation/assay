//! Who made the commits of a PR's range, for the eligibility rule's clause (b) (04 §5): every
//! GitHub-attributed author (co-authors included) and committer of a commit in
//! `base_sha..head_sha`, except clean merges from the base, and whether any commit has an author
//! or committer GitHub links to no user.

use std::collections::{BTreeSet, HashSet};

use crate::config::CommitId;
use crate::forge::{Actor, CommitIdentity};
use crate::status::UserId;

/// The identities of a PR's range as clause (b) of the eligibility rule reads them.
///
/// It has no `Default`: an empty value would turn clause (b) off, so the only constructor is
/// [`RangeIdentities::of`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeIdentities {
    users: BTreeSet<UserId>,
    unattributed: bool,
    clean_merges: HashSet<CommitId>,
}

impl RangeIdentities {
    /// The identities of `commits`, the range as Z4 resolved it from git objects, leaving out
    /// every commit in `clean_merges`: the clean merges from the base (two parents, the first the
    /// push's `before`, the second an ancestor of the base, a tree equal to a conflict-free
    /// merge), which the caller finds (04 §5). A push record whose `after` is one of them is left
    /// out of clause (c) too. `web-flow` as committer is a GitHub user, so it never makes a commit
    /// unattributed. A range with no counted commit counts as unattributed, so it leaves no
    /// identity eligible (fail closed): a real PR range is never empty.
    #[must_use]
    pub fn of(commits: &[CommitIdentity], clean_merges: HashSet<CommitId>) -> Self {
        let mut users = BTreeSet::new();
        let mut unattributed = false;
        let counted = commits
            .iter()
            .filter(|commit| !clean_merges.contains(&commit.commit));
        for commit in counted {
            for actor in commit.authors.iter().chain([&commit.committer]) {
                match actor {
                    Actor::User(user) => {
                        users.insert(*user);
                    }
                    Actor::NotAUser => unattributed = true,
                }
            }
        }
        Self {
            unattributed: unattributed || users.is_empty(),
            users,
            clean_merges,
        }
    }

    /// Whether `user` authored or committed a counted commit of the range.
    pub(super) fn made(&self, user: UserId) -> bool {
        self.users.contains(&user)
    }

    /// Whether a counted commit has an author or committer that is no GitHub user: such a commit
    /// counts as authored by every owner (04 §5).
    pub(super) const fn has_unattributed(&self) -> bool {
        self.unattributed
    }

    /// Whether `after` is a clean merge from the base, so its push record is not clause (c)'s.
    pub(super) fn is_clean_merge(&self, after: &CommitId) -> bool {
        self.clean_merges.contains(after)
    }
}
