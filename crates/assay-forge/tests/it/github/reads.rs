//! Reviews, labels, reactions and the timeline from recorded responses; an unknown review state
//! or reaction is refused, an unknown timeline kind is kept by name only.

use assay_app::{
    CommentId, Forge, Reaction, ReactionContent, ReactionSubject, Review, ReviewCommentId,
    ReviewState, TimelineEvent, TimelineKind, Timestamp, UserId,
};
use assay_forge::github::{ForgeError, MAX_LIST_ITEMS, Method, Operation};
use serde_json::json;

use super::recorded::{
    Recorded, Waits, fixture, github, head, id, json, owner, parent, path, pr, reply,
};

#[test]
fn a_list_longer_than_the_cap_is_refused_not_cut() {
    let page = |number: usize| {
        path(&format!(
            "issues/268/labels?per_page=100{}",
            if number == 1 {
                String::new()
            } else {
                format!("&page={number}")
            }
        ))
    };
    let labels: Vec<_> = (0..100)
        .map(|index| json!({ "name": format!("label-{index}") }))
        .collect();
    let body = json(&json!(labels));
    let pages = MAX_LIST_ITEMS / 100;
    let recorded = (1..=pages).fold(Recorded::default(), |recorded, number| {
        let next = format!(
            "<https://api.github.com/{}>; rel=\"next\"",
            page(number + 1)
        );
        recorded.then(
            Method::Get,
            &page(number),
            reply(200, &[("Link", &next)], body.clone()),
        )
    });
    let waits = Waits::default();

    let listed = github(&recorded, &waits).labels(pr());

    assert!(
        matches!(
            listed,
            Err(ForgeError::TooMany {
                operation: Operation::ListLabels,
                cap: MAX_LIST_ITEMS
            })
        ),
        "{listed:?}"
    );
    assert_eq!(recorded.sent().len(), pages, "no page past the cap is read");
}

#[test]
fn reviews_keep_state_reviewer_commit_and_submission_time() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls/268/reviews?per_page=100"),
        fixture("reviews.json"),
    );
    let waits = Waits::default();
    let submitted: Timestamp = "2026-10-18T21:40:56Z".parse().expect("a fixed time");

    let reviews = github(&recorded, &waits).reviews(pr());

    assert_eq!(
        reviews.expect("listed"),
        [
            Review {
                id: id(5_565_100_718),
                reviewer: Some(UserId(id(165_735_046))),
                state: ReviewState::Commented,
                commit: Some(head()),
                submitted_at: Some(submitted),
            },
            Review {
                id: id(5_565_100_719),
                reviewer: Some(owner()),
                state: ReviewState::Approved,
                commit: Some(head()),
                submitted_at: Some(submitted),
            },
            Review {
                id: id(5_565_100_720),
                reviewer: None,
                state: ReviewState::ChangesRequested,
                commit: Some(parent()),
                submitted_at: Some(submitted),
            },
        ]
    );
}

#[test]
fn a_pending_review_has_no_submission_time() {
    let body = json!([{
        "id": 9,
        "user": { "id": 124_330_161, "type": "User" },
        "state": "PENDING",
        "commit_id": null,
    }]);
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls/268/reviews?per_page=100"),
        body.to_string().into_bytes(),
    );
    let waits = Waits::default();

    let reviews = github(&recorded, &waits).reviews(pr());

    assert_eq!(
        reviews
            .expect("listed")
            .first()
            .map(|review| review.submitted_at),
        Some(None)
    );
}

#[test]
fn a_submission_time_that_is_not_utc_is_refused() {
    let body = json!([{
        "id": 9,
        "user": { "id": 124_330_161, "type": "User" },
        "state": "APPROVED",
        "commit_id": null,
        "submitted_at": "2026-10-18T21:40:56+01:00",
    }]);
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls/268/reviews?per_page=100"),
        body.to_string().into_bytes(),
    );
    let waits = Waits::default();

    let reviews = github(&recorded, &waits).reviews(pr());

    assert!(
        matches!(reviews, Err(ForgeError::Malformed { detail, .. }) if detail == "a review's submitted_at is not a UTC date-time"),
        "{reviews:?}"
    );
}

#[test]
fn an_unknown_review_state_is_refused() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls/268/reviews?per_page=100"),
        fixture("reviews_unknown_state.json"),
    );
    let waits = Waits::default();

    let reviews = github(&recorded, &waits).reviews(pr());

    assert!(
        matches!(reviews, Err(ForgeError::Malformed { detail, .. }) if detail == "unknown review state"),
        "{reviews:?}"
    );
}

#[test]
fn labels_are_their_names() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("issues/268/labels?per_page=100"),
        fixture("labels.json"),
    );
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    assert_eq!(labels.expect("listed"), ["assay:recheck", "bug"]);
}

#[test]
fn reactions_on_a_review_comment_keep_user_and_emoji() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls/comments/2400000001/reactions?per_page=100"),
        fixture("reactions.json"),
    );
    let waits = Waits::default();

    let reactions = github(&recorded, &waits).reactions(ReactionSubject::ReviewComment(
        ReviewCommentId(id(2_400_000_001)),
    ));

    assert_eq!(
        reactions.expect("listed"),
        [
            Reaction {
                id: id(301),
                user: Some(owner()),
                content: ReactionContent::ThumbsUp,
            },
            Reaction {
                id: id(302),
                user: None,
                content: ReactionContent::Eyes,
            },
        ]
    );
}

#[test]
fn an_unknown_reaction_is_refused() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("issues/comments/6255521954/reactions?per_page=100"),
        fixture("reactions_unknown.json"),
    );
    let waits = Waits::default();

    let reactions = github(&recorded, &waits)
        .reactions(ReactionSubject::IssueComment(CommentId(id(6_255_521_954))));

    assert!(
        matches!(reactions, Err(ForgeError::Malformed { detail, .. }) if detail == "unknown reaction"),
        "{reactions:?}"
    );
}

#[test]
fn the_timeline_types_what_assay_reads_and_names_the_rest() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("issues/268/timeline?per_page=100"),
        fixture("timeline.json"),
    );
    let waits = Waits::default();

    let timeline = github(&recorded, &waits).timeline(pr());

    let other = |name: &str| TimelineKind::Other(name.to_owned());
    let event = |event: Option<u64>, actor: Option<UserId>, kind| TimelineEvent {
        id: event.map(id),
        actor,
        kind,
    };
    let actions = Some(UserId(id(41_898_282)));
    let by_owner = Some(owner());
    assert_eq!(
        timeline.expect("read"),
        [
            event(None, None, other("committed")),
            event(Some(5_565_100_718), None, other("reviewed")),
            event(None, by_owner, other("cross-referenced")),
            event(Some(33_593_766_945), by_owner, other("assigned")),
            event(Some(6_255_521_952), actions, other("commented")),
            event(Some(33_602_149_310), by_owner, other("merged")),
            event(Some(33_602_149_439), by_owner, other("closed")),
            event(Some(33_602_150_979), by_owner, other("head_ref_deleted")),
            event(
                Some(33_602_150_990),
                by_owner,
                TimelineKind::Labeled("assay:recheck".to_owned())
            ),
            event(
                Some(33_602_150_991),
                by_owner,
                TimelineKind::ReviewDismissed(id(5_565_100_719))
            ),
        ]
    );
}
