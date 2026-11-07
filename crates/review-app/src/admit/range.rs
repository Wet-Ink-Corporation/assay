//! Who made the commits of a PR's range, for the eligibility rule's clause (b) (04 §5): every
//! GitHub-attributed author (co-authors included) and committer of a commit in
//! `base_sha..head_sha`, except clean merges from the base, and whether any commit has an author
//! or committer GitHub links to no user.

use std::collections::{BTreeSet, HashSet};

use crate::config::CommitId;
use crate::eligibility::Attribution;
use crate::forge::CommitIdentity;
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
    /// out of clause (c) too. Each counted commit is attributed by the one rule
    /// [`range_identities`](crate::range_identities) and decide's snapshot use (#347): every
    /// author, co-authors included, and the committer; `web-flow` is a GitHub user only as
    /// committer. A range with no counted commit counts as unattributed, so it leaves no
    /// identity eligible (fail closed): a real PR range is never empty.
    #[must_use]
    pub fn of(commits: &[CommitIdentity], clean_merges: HashSet<CommitId>) -> Self {
        let mut attribution = Attribution::default();
        commits
            .iter()
            .filter(|commit| !clean_merges.contains(&commit.commit))
            .for_each(|commit| attribution.count(commit));
        Self::attributed(attribution.users, attribution.unattributed, clean_merges)
    }

    /// The identities clause (b)'s one range reading attributed: `users` made a counted commit,
    /// and `unattributed` says whether one has an author or committer that is no GitHub user. As
    /// for [`Self::of`], a range with no counted commit counts as unattributed.
    pub(crate) fn attributed(
        users: BTreeSet<UserId>,
        unattributed: bool,
        clean_merges: HashSet<CommitId>,
    ) -> Self {
        Self {
            unattributed: unattributed || users.is_empty(),
            users,
            clean_merges,
        }
    }

    /// A range too large to attribute: it leaves no identity eligible (fail closed).
    pub(crate) fn unattributable() -> Self {
        Self::attributed(BTreeSet::new(), true, HashSet::new())
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
