//! Which reviews approve the head (04 §5 "Approvals", R3-23): per reviewer, the latest review
//! on `head_sha` that is not a comment must be `APPROVED`, not dismissed, submitted after the
//! push clock, by an eligible id.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use super::rule::{Ineligible, Rule};
use super::timestamp::Timestamp;
use crate::config::CommitId;
use crate::forge::{Review, ReviewState};
use crate::status::UserId;

/// An approval of the head that did not count because it was submitted before the push clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredApproval {
    /// The reviewer.
    pub reviewer: UserId,
    /// The review.
    pub review: NonZeroU64,
    /// When it was submitted.
    pub at: Timestamp,
    /// The push clock it did not postdate.
    pub push_at: Timestamp,
}

impl fmt::Display for IgnoredApproval {
    /// The summary's line: `approval at T ignored: before push at T′`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "approval at {} ignored: before push at {}",
            self.at, self.push_at
        )
    }
}

/// How a reviewer's standing approval of the head was judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovalStatus {
    /// It counts: `owner_approved(head)`.
    Counted,
    /// Submitted at or before the push clock.
    BeforePush(Timestamp),
    /// The head has no push record, so no approval can postdate its push.
    NoPushClock,
    /// GitHub gave no submission time.
    Unsubmitted,
    /// The reviewer is not eligible.
    Ineligible(Ineligible),
}

/// A reviewer's latest non-comment review of the head, when it is an approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Approval {
    pub(crate) review: NonZeroU64,
    pub(crate) reviewer: UserId,
    pub(crate) submitted_at: Option<Timestamp>,
    pub(crate) status: ApprovalStatus,
}

impl Approval {
    /// The summary's record of it, when it was ignored for timing.
    pub(crate) fn ignored(&self) -> Option<IgnoredApproval> {
        match (&self.status, &self.submitted_at) {
            (&ApprovalStatus::BeforePush(push_at), &Some(at)) => Some(IgnoredApproval {
                reviewer: self.reviewer,
                review: self.review,
                at,
                push_at,
            }),
            _ => None,
        }
    }
}

/// The standing approvals of `head`, one per reviewer, by reviewer id.
pub(crate) fn approvals(
    reviews: &[Review],
    head: &CommitId,
    clock: Option<Timestamp>,
    rule: &Rule,
) -> Vec<Approval> {
    let mut latest: BTreeMap<UserId, &Review> = BTreeMap::new();
    let standing = reviews.iter().filter(|review| {
        review.commit.as_ref() == Some(head)
            && !matches!(review.state, ReviewState::Commented | ReviewState::Pending)
    });
    for review in standing {
        let Some(reviewer) = review.reviewer else {
            continue;
        };
        latest
            .entry(reviewer)
            .and_modify(|kept| {
                if review.id > kept.id {
                    *kept = review;
                }
            })
            .or_insert(review);
    }
    latest
        .into_iter()
        .filter(|(_, review)| review.state == ReviewState::Approved)
        .map(|(reviewer, review)| Approval {
            review: review.id,
            reviewer,
            submitted_at: review.submitted_at,
            status: judge(review.submitted_at, clock, rule, reviewer),
        })
        .collect()
}

/// The status of an approval submitted at `submitted` by `reviewer`.
fn judge(
    submitted: Option<Timestamp>,
    clock: Option<Timestamp>,
    rule: &Rule,
    reviewer: UserId,
) -> ApprovalStatus {
    match (submitted, clock) {
        (None, _) => ApprovalStatus::Unsubmitted,
        (Some(_), None) => ApprovalStatus::NoPushClock,
        (Some(at), Some(clock)) if at <= clock => ApprovalStatus::BeforePush(clock),
        (Some(_), Some(_)) => match rule.judge(reviewer) {
            Ok(()) => ApprovalStatus::Counted,
            Err(why) => ApprovalStatus::Ineligible(why),
        },
    }
}
