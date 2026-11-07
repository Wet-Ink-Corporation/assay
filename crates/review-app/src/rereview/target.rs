//! What a re-review reviews and what it carries (`docs/architecture/04-pipeline.md` §7): the
//! target is always the full cumulative diff; the increment since the last completed run is
//! marked from `git range-diff` from the pinned base when the previous head is reachable, and the findings it left
//! open are carried only while that head is an ancestor of head.

use std::fmt;

use thiserror::Error;

use super::carried::CarriedFinding;
use crate::config::CommitId;

/// The most bytes of `git range-diff` output marked as the increment (the context pack's diff
/// budget, 04 §3); a longer one leaves the target unmarked.
pub const MAX_RANGE_DIFF_BYTES: usize = 120 * 1024;

/// The PR's history, read from the object repository. Implemented by the forge adapter.
pub trait PriorHistory {
    /// Why the repository could not be read.
    type Error: std::error::Error;

    /// How `previous`, the head of the last completed run, stands to `head`.
    ///
    /// # Errors
    /// [`Self::Error`] when git cannot run or `head` is missing; an absent `previous` is
    /// [`Ancestry::Unreachable`], not an error.
    fn ancestry(&self, previous: &CommitId, head: &CommitId) -> Result<Ancestry, Self::Error>;

    /// `git range-diff <base> <previous> <head>` of `commits`, its output read under `limit`
    /// bytes.
    ///
    /// # Errors
    /// [`Self::Error`] when git cannot run or fails; output past `limit` or too many commits
    /// is [`RangeDiffListing::TooLarge`], and a timeout [`RangeDiffListing::TooSlow`], not
    /// errors.
    fn range_diff(
        &self,
        commits: RangeDiffCommits<'_>,
        limit: usize,
    ) -> Result<RangeDiffListing, Self::Error>;
}

/// The three commits a range-diff compares: `base..previous` against `base..head`.
#[derive(Debug, Clone, Copy)]
pub struct RangeDiffCommits<'a> {
    /// The pinned base.
    pub base: &'a CommitId,
    /// The last completed run's head.
    pub previous: &'a CommitId,
    /// This run's head.
    pub head: &'a CommitId,
}

/// How the last completed run's head stands to this run's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ancestry {
    /// It is an ancestor of head (or head itself): the PR only gained commits.
    Ancestor,
    /// It is reachable but not an ancestor: the PR was rebased or force-pushed.
    Rewritten,
    /// Its commit cannot be fetched or is not in the repository.
    Unreachable,
}

/// What `git range-diff` printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeDiffListing {
    /// Its output, within the limit.
    Listed(Vec<u8>),
    /// It compared too many commits, or its output was longer than the limit, and was not
    /// kept.
    TooLarge,
    /// It did not finish within its time limit.
    TooSlow,
}

/// The run being planned: its pinned SHAs and the head of the PR's last completed run.
#[derive(Debug, Clone, Copy)]
pub struct TargetRequest<'a> {
    /// The pinned base SHA.
    pub base: &'a CommitId,
    /// The pinned head SHA.
    pub head: &'a CommitId,
    /// The head SHA of the PR's last completed run, from the ledger; `None` before any.
    pub previous: Option<&'a CommitId>,
}

/// `git range-diff` output marking what is new since the last review.
///
/// It is PR-controlled text, and it holds the commits' subjects and messages: those are the
/// PR's intent, which 04 §3 shows only to `design` and `meta`, so whoever puts it in a prompt
/// keeps it inside the intent envelope.
#[derive(Clone, PartialEq, Eq)]
pub struct RangeDiff(String);

impl RangeDiff {
    /// The output, as git printed it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RangeDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RangeDiff")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// How the increment since the last completed run is marked. The specialists review the
/// cumulative diff either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Increment {
    /// The range-diff from the previous head is marked "new since last review".
    Since {
        /// The last completed run's head.
        previous: CommitId,
        /// What changed since it.
        range_diff: RangeDiff,
    },
    /// Nothing is marked: the cumulative diff is all new.
    Cumulative(Cumulative),
}

/// Why nothing is marked as new since the last review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cumulative {
    /// There is no completed run.
    FirstReview,
    /// The previous head cannot be reached, for example after a force-push dropped it.
    Unreachable,
    /// The range-diff could not mark it: it failed, ran too long, compared too many commits,
    /// printed more than [`MAX_RANGE_DIFF_BYTES`] or is not UTF-8.
    Unavailable,
}

/// Whether the findings the last completed run left open are carried into this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Carry {
    /// There is no completed run, so nothing to carry.
    FirstReview,
    /// The previous head is an ancestor of head: its open findings are carried.
    Kept,
    /// The carry-over set is reset; dismissal records are not touched.
    Reset(CarryReset),
}

/// Why the carry-over set is reset (04 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CarryReset {
    /// The PR was rebased or force-pushed.
    Rewritten,
    /// The previous head cannot be reached.
    Unreachable,
}

/// What this run reviews and carries. `E` is the history port's error, kept when the
/// range-diff failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewTarget<E> {
    increment: Increment,
    carry: Carry,
    range_diff_failure: Option<E>,
}

impl<E> ReviewTarget<E> {
    /// How the increment is marked in the cumulative target.
    #[must_use]
    pub const fn increment(&self) -> &Increment {
        &self.increment
    }

    /// Whether the last run's open findings are carried.
    #[must_use]
    pub const fn carry(&self) -> Carry {
        self.carry
    }

    /// Why the range-diff failed, when it did: the increment is then
    /// [`Cumulative::Unavailable`]. The caller decides whether the error also aborts the job
    /// (the forge's `ProcessError::Kill` does: a process group may still be running).
    #[must_use]
    pub const fn range_diff_failure(&self) -> Option<&E> {
        self.range_diff_failure.as_ref()
    }

    /// The findings this run carries: all of `open` when [`Carry::Kept`], otherwise none.
    #[must_use]
    pub fn carried(&self, open: Vec<CarriedFinding>) -> Vec<CarriedFinding> {
        match self.carry {
            Carry::Kept => open,
            Carry::FirstReview | Carry::Reset(_) => Vec::new(),
        }
    }
}

/// How the previous head stands to head could not be read, so the carry-over is not known.
#[derive(Debug, Error)]
#[error("could not read how the previous head stands to head")]
pub struct TargetError<E> {
    /// Why.
    #[source]
    pub source: E,
}

/// Decides how `request`'s run marks its increment and whether it carries the last completed
/// run's open findings (04 §7), reading `history`.
///
/// With no completed run nothing is marked or carried and `history` is not read. An unreachable
/// previous head marks nothing and resets the carry-over. A reachable one is range-diffed
/// against head from the pinned base; the carry-over is kept only when it is an ancestor of
/// head. The range-diff only marks: when it fails, is too large, too slow or not UTF-8 the
/// increment is [`Cumulative::Unavailable`] and the carry decision stands.
///
/// # Errors
/// [`TargetError`] when the ancestry could not be read.
pub fn review_target<H: PriorHistory>(
    request: &TargetRequest<'_>,
    history: &H,
) -> Result<ReviewTarget<H::Error>, TargetError<H::Error>> {
    let Some(previous) = request.previous else {
        return Ok(ReviewTarget {
            increment: Increment::Cumulative(Cumulative::FirstReview),
            carry: Carry::FirstReview,
            range_diff_failure: None,
        });
    };
    let ancestry = history
        .ancestry(previous, request.head)
        .map_err(|source| TargetError { source })?;
    let carry = match ancestry {
        Ancestry::Ancestor => Carry::Kept,
        Ancestry::Rewritten => Carry::Reset(CarryReset::Rewritten),
        Ancestry::Unreachable => {
            return Ok(ReviewTarget {
                increment: Increment::Cumulative(Cumulative::Unreachable),
                carry: Carry::Reset(CarryReset::Unreachable),
                range_diff_failure: None,
            });
        }
    };
    let commits = RangeDiffCommits {
        base: request.base,
        previous,
        head: request.head,
    };
    let (increment, range_diff_failure) = match history.range_diff(commits, MAX_RANGE_DIFF_BYTES) {
        Ok(listing) => (marked(previous, listing), None),
        Err(failure) => (
            Increment::Cumulative(Cumulative::Unavailable),
            Some(failure),
        ),
    };
    Ok(ReviewTarget {
        increment,
        carry,
        range_diff_failure,
    })
}

/// The increment `listing` marks from `previous`: unmarked unless it is UTF-8 within the cap.
fn marked(previous: &CommitId, listing: RangeDiffListing) -> Increment {
    match listing {
        RangeDiffListing::Listed(bytes) if bytes.len() <= MAX_RANGE_DIFF_BYTES => {
            match String::from_utf8(bytes) {
                Ok(text) => Increment::Since {
                    // The increment names the commit it is measured from.
                    previous: previous.clone(),
                    range_diff: RangeDiff(text),
                },
                Err(_not_utf8) => Increment::Cumulative(Cumulative::Unavailable),
            }
        }
        RangeDiffListing::Listed(_) | RangeDiffListing::TooLarge | RangeDiffListing::TooSlow => {
            Increment::Cumulative(Cumulative::Unavailable)
        }
    }
}
