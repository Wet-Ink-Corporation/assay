//! #332 (#331 F3, #318 F6): `decide`'s publication calls, made under the publish lock, take its
//! lease's expiry as a deadline as `open`'s do (#290): a call whose deadline has come sends
//! nothing, and a retry that would pass it is neither waited for nor sent.

use std::time::Duration;

use assay_app::{
    CheckName, CheckRunId, CheckState, CheckUpdate, CommentForge, CommentId, Deadline,
    InlineComment, LockedWrite, SUMMARY_MARKER, VerdictForge,
};
use assay_domain::Side;
use assay_forge::github::{ForgeError, Method, Operation};

use super::recorded::{
    Recorded, SEND_TIMEOUT, Unscripted, Waits, deadline_after, github, head, id, path, pr, reply,
};

fn verdict() -> CheckName {
    CheckName::for_base("main", "main")
}

fn decision() -> CheckUpdate {
    CheckUpdate {
        state: CheckState::InProgress,
        title: "assay: pending".to_owned(),
        summary: "Review in progress.".to_owned(),
        text: String::new(),
        external_id: format!("sha256:{}", "a".repeat(64)),
    }
}

/// What the adapter refused for `operation` when the deadline stopped it.
fn past(error: &ForgeError<Unscripted>, operation: Operation) -> bool {
    matches!(error, ForgeError::PastDeadline { operation: refused } if *refused == operation)
}

/// Every call of `VerdictForge` and `CommentForge` under `deadline`, with the operation the
/// adapter names when it refuses it.
fn every_call(deadline: Deadline) -> Vec<(Operation, Result<(), ForgeError<Unscripted>>)> {
    let recorded = Recorded::default();
    let waits = Waits::default();
    let forge = github(&recorded, &waits);
    let update = decision();
    let write = LockedWrite {
        update: &update,
        deadline,
    };
    let comment = InlineComment {
        commit_id: &head(),
        path: "src/lib.rs",
        line: id(3),
        start_line: None,
        side: Side::Right,
        body: "<!-- assay-finding 0000000000000001 -->".to_owned(),
    };
    let check = CheckRunId(id(117_798_470_001));
    let calls = vec![
        (
            Operation::ListChecks,
            forge
                .find_check(&head(), &verdict(), Some(deadline))
                .map(drop),
        ),
        (Operation::UpdateCheck, forge.update_check(check, write)),
        (
            Operation::ListComments,
            forge.find_summary(pr(), SUMMARY_MARKER, deadline).map(drop),
        ),
        (
            Operation::CreateComment,
            forge.create_summary(pr(), SUMMARY_MARKER, deadline),
        ),
        (
            Operation::UpdateComment,
            forge.update_summary(CommentId(id(6_255_521_954)), SUMMARY_MARKER, deadline),
        ),
        (
            Operation::ListReviewComments,
            forge.app_review_comments(pr(), deadline).map(drop),
        ),
        (
            Operation::CreateReviewComment,
            forge.create_review_comment(pr(), &comment, deadline),
        ),
        (
            Operation::CreateCheck,
            forge.create_check(&head(), &verdict(), write),
        ),
    ];
    assert_eq!(recorded.sent(), Vec::new(), "nothing is sent");
    calls
}

#[test]
fn every_publication_call_whose_deadline_has_come_sends_nothing() {
    for (operation, result) in every_call(deadline_after(Duration::ZERO)) {
        let error = result.expect_err("the deadline has come");
        assert!(past(&error, operation), "{operation:?}: {error:?}");
    }
}

#[test]
fn a_publication_call_that_could_outlast_the_deadline_by_its_timeout_sends_nothing() {
    for (operation, result) in every_call(deadline_after(SEND_TIMEOUT)) {
        let error = result.expect_err("ending exactly at the deadline is past it");
        assert!(past(&error, operation), "{operation:?}: {error:?}");
    }
}

#[test]
fn a_summary_post_whose_retry_would_pass_the_deadline_is_neither_probed_nor_posted_again() {
    let recorded = Recorded::default().then(
        Method::Post,
        &path("issues/268/comments"),
        reply(502, &[], Vec::new()),
    );
    let waits = Waits::default();
    let deadline = deadline_after(SEND_TIMEOUT + Duration::from_millis(500));

    let created = github(&recorded, &waits).create_summary(pr(), SUMMARY_MARKER, deadline);

    let error = created.expect_err("the retry would pass the deadline");
    assert!(past(&error, Operation::CreateComment), "{error:?}");
    assert_eq!(waits.taken(), Vec::new(), "no wait taken");
    assert_eq!(
        recorded.sent().len(),
        1,
        "one POST, no probe, never repeated"
    );
}

/// A deadline that admits the first POST at 0 s, the 1 s wait and the probe at 1 s, but not
/// the probe's own 1 s retry wait (1 s + 1 s + the 30 s timeout is past 31.5 s).
fn probe_deadline() -> Deadline {
    deadline_after(SEND_TIMEOUT + Duration::from_millis(1_500))
}

#[test]
fn the_probe_before_a_summary_post_retry_is_bounded_by_the_same_deadline() {
    let recorded = Recorded::default()
        .then(
            Method::Post,
            &path("issues/268/comments"),
            reply(502, &[], Vec::new()),
        )
        .then(
            Method::Get,
            &path("issues/268/comments?per_page=100"),
            reply(502, &[], Vec::new()),
        );
    let waits = Waits::default();

    let created = github(&recorded, &waits).create_summary(pr(), SUMMARY_MARKER, probe_deadline());

    let error = created.expect_err("the probe's retry would pass the deadline");
    assert!(past(&error, Operation::ListComments), "{error:?}");
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
    assert_eq!(recorded.sent().len(), 2, "the POST, then the probe");
}

#[test]
fn the_probe_before_a_review_comment_post_retry_is_bounded_by_the_same_deadline() {
    let recorded = Recorded::default()
        .then(
            Method::Post,
            &path("pulls/268/comments"),
            reply(502, &[], Vec::new()),
        )
        .then(
            Method::Get,
            &path("pulls/268/comments?per_page=100"),
            reply(502, &[], Vec::new()),
        );
    let waits = Waits::default();
    let comment = InlineComment {
        commit_id: &head(),
        path: "src/lib.rs",
        line: id(3),
        start_line: None,
        side: Side::Right,
        body: "<!-- assay-finding 0000000000000001 -->".to_owned(),
    };

    let created = github(&recorded, &waits).create_review_comment(pr(), &comment, probe_deadline());

    let error = created.expect_err("the probe's retry would pass the deadline");
    assert!(past(&error, Operation::ListReviewComments), "{error:?}");
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
    assert_eq!(recorded.sent().len(), 2, "the POST, then the probe");
}
