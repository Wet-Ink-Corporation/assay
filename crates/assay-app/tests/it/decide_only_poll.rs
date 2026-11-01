//! The hourly decide-only poll's selection (#61 DO-10, D8 A) over a fake [`PollForge`]: open PRs
//! whose `assay-verdict` check is a `failure` titled exactly `trust-path-unapproved` (as #218
//! reads titles) with an `APPROVED` review on the head submitted after the check completed; at
//! most [`MAX_POLL_SELECTED`], oldest check first.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use assay_app::{
    CheckName, CommitId, CompletedVerdict, MAX_POLL_PULLS, MAX_POLL_SELECTED, ObservedConclusion,
    PollError, PollForge, PolledPull, Review, ReviewState, Timestamp, VerdictView, poll_pending,
};

const UNAPPROVED: &str = "assay: failure (trust-path-unapproved)";

fn id(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("non-zero")
}

/// PR `n`'s head: `n` in hex, padded to a full commit id.
fn head(n: u64) -> CommitId {
    format!("{n:040x}").parse().expect("a full commit id")
}

/// `2026-10-31T10:<minute>:00Z`.
fn at(minute: u32) -> Timestamp {
    format!("2026-10-31T10:{minute:02}:00Z")
        .parse()
        .expect("a UTC time")
}

/// A completed check.
fn completed(conclusion: ObservedConclusion, title: Option<&str>, minute: u32) -> VerdictView {
    VerdictView::Completed(CompletedVerdict {
        conclusion,
        title: title.map(str::to_owned),
        completed_at: at(minute),
    })
}

/// The check the poll is looking for, completed at `minute`.
fn unapproved(minute: u32) -> VerdictView {
    completed(ObservedConclusion::Failure, Some(UNAPPROVED), minute)
}

/// A review of PR `n`.
fn review(state: ReviewState, commit: Option<CommitId>, minute: Option<u32>) -> Review {
    Review {
        id: id(9_000),
        reviewer: Some(assay_app::UserId(id(583_231))),
        state,
        commit,
        submitted_at: minute.map(at),
    }
}

/// An approval of PR `n`'s head at `minute`.
fn approved(n: u64, minute: u32) -> Review {
    review(ReviewState::Approved, Some(head(n)), Some(minute))
}

/// A port failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Failed;

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("failed")
    }
}

impl std::error::Error for Failed {}

/// One open PR: its base, its App check (if any) and its reviews.
struct Pull {
    base: &'static str,
    check: Option<VerdictView>,
    reviews: Vec<Review>,
}

/// The poll's reads, from a table of open PRs in GitHub's order.
#[derive(Default)]
struct FakePoll {
    pulls: Vec<(u64, Pull)>,
    fail_pulls: bool,
    fail_check: Option<u64>,
    fail_reviews: Option<u64>,
    /// The limit `open_pulls` was called with, and each PR whose reviews were read.
    calls: RefCell<(Option<usize>, Vec<u64>)>,
}

impl FakePoll {
    fn with(mut self, n: u64, check: Option<VerdictView>, reviews: Vec<Review>) -> Self {
        self.pulls.push((
            n,
            Pull {
                base: "main",
                check,
                reviews,
            },
        ));
        self
    }

    fn into_base(mut self, n: u64, base: &'static str) -> Self {
        for (number, pull) in &mut self.pulls {
            if *number == n {
                pull.base = base;
            }
        }
        self
    }

    fn pull(&self, head_sha: &CommitId) -> Option<(u64, &Pull)> {
        self.pulls
            .iter()
            .find(|(n, _)| head(*n) == *head_sha)
            .map(|(n, pull)| (*n, pull))
    }
}

impl PollForge for FakePoll {
    type Error = Failed;

    fn open_pulls(&self, limit: usize) -> Result<Vec<PolledPull>, Failed> {
        self.calls.borrow_mut().0 = Some(limit);
        if self.fail_pulls {
            return Err(Failed);
        }
        Ok(self
            .pulls
            .iter()
            .map(|(n, pull)| PolledPull {
                number: id(*n),
                head_sha: head(*n),
                base_ref: pull.base.to_owned(),
            })
            .collect())
    }

    fn verdict_check(
        &self,
        head: &CommitId,
        name: &CheckName,
    ) -> Result<Option<VerdictView>, Failed> {
        let (n, pull) = self.pull(head).expect("a listed head");
        if self.fail_check == Some(n) {
            return Err(Failed);
        }
        // The App's check is named for the PR's base; any other name finds nothing.
        let expected = CheckName::for_base(pull.base, "main");
        Ok(pull.check.clone().filter(|_| *name == expected))
    }

    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, Failed> {
        self.calls.borrow_mut().1.push(number.get());
        if self.fail_reviews == Some(number.get()) {
            return Err(Failed);
        }
        let (_, pull) = self
            .pulls
            .iter()
            .find(|(n, _)| *n == number.get())
            .expect("a listed PR");
        Ok(pull.reviews.clone())
    }
}

fn selected(poll: &FakePoll) -> Vec<u64> {
    poll_pending(poll, "main")
        .expect("the poll reads")
        .into_iter()
        .map(NonZeroU64::get)
        .collect()
}

#[test]
fn an_unapproved_trust_path_failure_approved_on_its_head_after_the_check_is_selected() {
    let poll = FakePoll::default().with(2, Some(unapproved(10)), vec![approved(2, 11)]);

    let prs = selected(&poll);

    assert_eq!(prs, [2]);
    assert_eq!(poll.calls.borrow().0, Some(MAX_POLL_PULLS));
}

#[test]
fn a_check_that_is_not_that_failure_is_passed_over_without_reading_reviews() {
    let checks = [
        None,
        Some(VerdictView::Pending),
        Some(completed(
            ObservedConclusion::Success,
            Some("assay: success"),
            10,
        )),
        Some(completed(ObservedConclusion::Neutral, Some(UNAPPROVED), 10)),
        Some(completed(ObservedConclusion::Failure, None, 10)),
        Some(completed(
            ObservedConclusion::Failure,
            Some("assay: failure (gate-error)"),
            10,
        )),
        Some(completed(
            ObservedConclusion::Failure,
            Some("assay: failure (trust-path-unapproved) "),
            10,
        )),
        Some(completed(
            ObservedConclusion::Failure,
            Some("assay: failure (trust-path-unapproved, gate-error)"),
            10,
        )),
        Some(completed(
            ObservedConclusion::Failure,
            Some("trust-path-unapproved"),
            10,
        )),
    ];
    for check in checks {
        let poll = FakePoll::default().with(2, check.clone(), vec![approved(2, 11)]);

        let prs = selected(&poll);

        assert_eq!(prs, [0_u64; 0], "{check:?}");
        assert_eq!(poll.calls.borrow().1, [0_u64; 0], "{check:?}");
    }
}

#[test]
fn only_an_approval_of_the_head_submitted_after_the_check_selects_it() {
    let reviews = [
        approved(2, 9),
        approved(2, 10),
        review(ReviewState::Approved, Some(head(3)), Some(11)),
        review(ReviewState::Approved, None, Some(11)),
        review(ReviewState::Approved, Some(head(2)), None),
        review(ReviewState::ChangesRequested, Some(head(2)), Some(11)),
        review(ReviewState::Commented, Some(head(2)), Some(11)),
        review(ReviewState::Dismissed, Some(head(2)), Some(11)),
        review(ReviewState::Pending, Some(head(2)), Some(11)),
    ];
    for review in reviews {
        let poll = FakePoll::default().with(2, Some(unapproved(10)), vec![review.clone()]);

        let prs = selected(&poll);

        assert_eq!(prs, [0_u64; 0], "{review:?}");
    }
}

#[test]
fn a_pr_into_another_base_is_found_by_that_base_s_check_name() {
    let poll = FakePoll::default()
        .with(4, Some(unapproved(10)), vec![approved(4, 11)])
        .into_base(4, "release");

    let prs = selected(&poll);

    assert_eq!(prs, [4]);
}

#[test]
fn at_most_twenty_are_selected_oldest_check_first() {
    let mut poll = FakePoll::default();
    // 25 pending PRs, listed newest PR first; checks complete in a shuffled order, PRs 30 and
    // 31 at the same minute (the lower number first).
    for n in (10..35).rev() {
        let minute = u32::try_from((n * 7) % 25).expect("small");
        poll = poll.with(n, Some(unapproved(minute)), vec![approved(n, 59)]);
    }
    let mut expected: Vec<(u32, u64)> = (10..35)
        .map(|n| (u32::try_from((n * 7) % 25).expect("small"), n))
        .collect();
    expected.sort_unstable();
    let expected: Vec<u64> = expected
        .into_iter()
        .map(|(_, n)| n)
        .take(MAX_POLL_SELECTED)
        .collect();

    let prs = selected(&poll);

    assert_eq!(prs.len(), MAX_POLL_SELECTED);
    assert_eq!(prs, expected);
}

#[test]
fn checks_completed_at_the_same_time_are_taken_in_pr_order() {
    let poll = FakePoll::default()
        .with(9, Some(unapproved(10)), vec![approved(9, 11)])
        .with(3, Some(unapproved(10)), vec![approved(3, 11)])
        .with(5, Some(unapproved(8)), vec![approved(5, 11)]);

    let prs = selected(&poll);

    assert_eq!(prs, [5, 3, 9]);
}

#[test]
fn nothing_pending_selects_nothing() {
    let poll = FakePoll::default()
        .with(2, Some(unapproved(10)), Vec::new())
        .with(3, None, Vec::new());

    let prs = selected(&poll);

    assert_eq!(prs, [0_u64; 0]);
    assert_eq!(selected(&FakePoll::default()), [0_u64; 0]);
}

#[test]
fn each_read_s_failure_is_reported_with_its_pr() {
    let pulls = FakePoll {
        fail_pulls: true,
        ..FakePoll::default()
    };
    let mut check = FakePoll::default().with(2, Some(unapproved(10)), vec![approved(2, 11)]);
    check.fail_check = Some(2);
    let mut reviews = FakePoll::default().with(3, Some(unapproved(10)), vec![approved(3, 11)]);
    reviews.fail_reviews = Some(3);

    assert_eq!(poll_pending(&pulls, "main"), Err(PollError::Pulls(Failed)));
    assert_eq!(
        poll_pending(&check, "main"),
        Err(PollError::Check {
            pr: id(2),
            source: Failed
        })
    );
    assert_eq!(
        poll_pending(&reviews, "main"),
        Err(PollError::Reviews {
            pr: id(3),
            source: Failed
        })
    );
}

#[test]
fn a_poll_error_names_the_read_and_its_pr() {
    let error = PollError::Reviews {
        pr: id(3),
        source: Failed,
    };

    assert_eq!(error.to_string(), "reading PR #3's reviews failed");
    assert_eq!(
        PollError::Check {
            pr: id(2),
            source: Failed
        }
        .to_string(),
        "reading PR #2's verdict check failed"
    );
    assert_eq!(
        PollError::Pulls(Failed).to_string(),
        "listing the open PRs failed"
    );
}

/// An approval at the check's own time is not after it; approved later, the PRs come out by
/// completion time, not in the listing's order (PR number here).
#[test]
fn the_selection_follows_completion_times_not_listing_order() {
    let minutes: BTreeMap<u64, u32> = [(2, 30), (3, 5), (4, 59), (5, 0)].into_iter().collect();
    let mut poll = FakePoll::default();
    for (&n, &minute) in &minutes {
        poll = poll.with(n, Some(unapproved(minute)), vec![approved(n, minute)]);
    }
    // Each approval is at the check's own minute, so none is after it.
    assert_eq!(selected(&poll), [0_u64; 0]);

    let mut poll = FakePoll::default();
    for (&n, &minute) in &minutes {
        poll = poll.with(
            n,
            Some(unapproved(minute)),
            vec![review(ReviewState::Approved, Some(head(n)), Some(59))],
        );
    }

    assert_eq!(selected(&poll), [5, 3, 2]);
}
