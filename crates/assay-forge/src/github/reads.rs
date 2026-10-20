//! Reads for the GitHub state snapshot and for feedback: reviews, labels, reactions and the
//! timeline. An unknown review state or reaction is refused; an unknown timeline kind is kept
//! by name only.

use std::num::NonZeroU64;

use assay_app::{
    Reaction, ReactionContent, ReactionSubject, Review, ReviewState, TimelineEvent, TimelineKind,
};

use super::GitHub;
use super::api::PER_PAGE;
use super::dto::{self, LabelDto, ReactionDto, ReviewDto, TimelineDto};
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// Every review on PR `number`.
    pub(crate) fn list_reviews(
        &self,
        number: NonZeroU64,
    ) -> Result<Vec<Review>, ForgeError<T::Error>> {
        let operation = Operation::ListReviews;
        let path = self.path(
            operation,
            &format!("pulls/{number}/reviews?per_page={PER_PAGE}"),
        )?;
        self.api
            .list::<Vec<ReviewDto>>(operation, path, None)?
            .into_iter()
            .map(|review| {
                Ok(Review {
                    id: dto::nonzero(operation, review.id, "a review id is zero")?,
                    reviewer: dto::user(operation, review.user.as_ref())?,
                    state: review_state(operation, &review.state)?,
                    commit: review
                        .commit_id
                        .as_deref()
                        .map(|sha| dto::commit(operation, sha))
                        .transpose()?,
                    submitted_at: review
                        .submitted_at
                        .as_deref()
                        .map(|at| {
                            at.parse().map_err(|_not_utc| ForgeError::Malformed {
                                operation,
                                detail: "a review's submitted_at is not a UTC date-time",
                            })
                        })
                        .transpose()?,
                })
            })
            .collect()
    }

    /// The names of the labels on PR `number`.
    pub(crate) fn list_labels(
        &self,
        number: NonZeroU64,
    ) -> Result<Vec<String>, ForgeError<T::Error>> {
        let operation = Operation::ListLabels;
        let path = self.path(
            operation,
            &format!("issues/{number}/labels?per_page={PER_PAGE}"),
        )?;
        self.api
            .list::<Vec<LabelDto>>(operation, path, None)?
            .into_iter()
            .map(|label| label_name(operation, label))
            .collect()
    }

    /// Every reaction on `subject`.
    pub(crate) fn list_reactions(
        &self,
        subject: ReactionSubject,
    ) -> Result<Vec<Reaction>, ForgeError<T::Error>> {
        let operation = Operation::ListReactions;
        let comment = match subject {
            ReactionSubject::IssueComment(id) => format!("issues/comments/{}", id.0),
            ReactionSubject::ReviewComment(id) => format!("pulls/comments/{}", id.0),
        };
        let path = self.path(
            operation,
            &format!("{comment}/reactions?per_page={PER_PAGE}"),
        )?;
        self.api
            .list::<Vec<ReactionDto>>(operation, path, None)?
            .into_iter()
            .map(|reaction| {
                Ok(Reaction {
                    id: dto::nonzero(operation, reaction.id, "a reaction id is zero")?,
                    user: dto::user(operation, reaction.user.as_ref())?,
                    content: reaction_content(operation, &reaction.content)?,
                })
            })
            .collect()
    }

    /// PR `number`'s timeline.
    pub(crate) fn read_timeline(
        &self,
        number: NonZeroU64,
    ) -> Result<Vec<TimelineEvent>, ForgeError<T::Error>> {
        let operation = Operation::ReadTimeline;
        let path = self.path(
            operation,
            &format!("issues/{number}/timeline?per_page={PER_PAGE}"),
        )?;
        self.api
            .list::<Vec<TimelineDto>>(operation, path, None)?
            .into_iter()
            .map(|event| timeline_event(operation, event))
            .collect()
    }
}

/// A label's name, refused when empty.
fn label_name<E>(operation: Operation, label: LabelDto) -> Result<String, ForgeError<E>> {
    if label.name.is_empty() {
        return Err(ForgeError::Malformed {
            operation,
            detail: "a label has no name",
        });
    }
    Ok(label.name)
}

/// A review's `state`.
fn review_state<E>(operation: Operation, state: &str) -> Result<ReviewState, ForgeError<E>> {
    match state {
        "APPROVED" => Ok(ReviewState::Approved),
        "CHANGES_REQUESTED" => Ok(ReviewState::ChangesRequested),
        "COMMENTED" => Ok(ReviewState::Commented),
        "DISMISSED" => Ok(ReviewState::Dismissed),
        "PENDING" => Ok(ReviewState::Pending),
        _ => Err(ForgeError::Malformed {
            operation,
            detail: "unknown review state",
        }),
    }
}

/// A reaction's `content`.
fn reaction_content<E>(
    operation: Operation,
    content: &str,
) -> Result<ReactionContent, ForgeError<E>> {
    match content {
        "+1" => Ok(ReactionContent::ThumbsUp),
        "-1" => Ok(ReactionContent::ThumbsDown),
        "laugh" => Ok(ReactionContent::Laugh),
        "confused" => Ok(ReactionContent::Confused),
        "heart" => Ok(ReactionContent::Heart),
        "hooray" => Ok(ReactionContent::Hooray),
        "rocket" => Ok(ReactionContent::Rocket),
        "eyes" => Ok(ReactionContent::Eyes),
        _ => Err(ForgeError::Malformed {
            operation,
            detail: "unknown reaction",
        }),
    }
}

/// One timeline event: a kind assay reads must carry its data; any other kind is kept by name.
fn timeline_event<E>(
    operation: Operation,
    event: TimelineDto,
) -> Result<TimelineEvent, ForgeError<E>> {
    let missing = |detail| ForgeError::Malformed { operation, detail };
    let known = match event.event.as_str() {
        "labeled" | "unlabeled" => {
            let label = event
                .label
                .ok_or_else(|| missing("a label event names no label"))?;
            let name = label_name(operation, label)?;
            Some(if event.event == "labeled" {
                TimelineKind::Labeled(name)
            } else {
                TimelineKind::Unlabeled(name)
            })
        }
        "review_dismissed" => {
            let dismissed = event
                .dismissed_review
                .ok_or_else(|| missing("a dismissal names no review"))?;
            Some(TimelineKind::ReviewDismissed(dto::nonzero(
                operation,
                dismissed.review_id,
                "a review id is zero",
            )?))
        }
        "head_ref_force_pushed" => Some(TimelineKind::HeadRefForcePushed),
        "base_ref_changed" => Some(TimelineKind::BaseRefChanged),
        "ready_for_review" => Some(TimelineKind::ReadyForReview),
        "convert_to_draft" => Some(TimelineKind::ConvertToDraft),
        "" => return Err(missing("a timeline event has no kind")),
        _ => None,
    };
    let kind = known.unwrap_or(TimelineKind::Other(event.event));
    Ok(TimelineEvent {
        id: event
            .id
            .map(|id| dto::nonzero(operation, id, "an event id is zero"))
            .transpose()?,
        actor: dto::user(operation, event.actor.as_ref())?,
        kind,
    })
}
