//! Calls made under the publish lock take its lease's expiry as a deadline (#290): no request is
//! sent, and no retry waited for, unless it would end before it, measured on the [`Waits`]
//! clock, which moves only by the waits taken, and the transport's own timeout.

use std::time::Duration;

use assay_app::{
    CheckName, CheckRunId, CheckState, CheckUpdate, CompareAndSetError, Deadline, Forge,
    LockedWrite,
};
use assay_forge::github::{ForgeError, Method, Operation};

use super::recorded::{
    HEAD, Recorded, SEND_TIMEOUT, Waits, deadline_after, fixture, github, head, id, path, reply,
};

fn verdict() -> CheckName {
    CheckName::for_base("main", "main")
}

fn listing() -> String {
    path(&format!(
        "commits/{HEAD}/check-runs?filter=all&per_page=100&check_name=assay%20%2F%20verdict"
    ))
}

fn pending() -> CheckUpdate {
    CheckUpdate {
        state: CheckState::InProgress,
        title: "assay: pending".to_owned(),
        summary: "Review in progress.".to_owned(),
        text: String::new(),
        external_id: "pending:39168909542:3".to_owned(),
    }
}

fn check() -> CheckRunId {
    CheckRunId(id(117_798_470_001))
}

/// `update` written under a lock whose lease expires at `deadline`.
fn by(update: &CheckUpdate, deadline: Deadline) -> LockedWrite<'_> {
    LockedWrite { update, deadline }
}

/// What the adapter refused for `operation` when the deadline stopped it.
fn past(error: &ForgeError<super::recorded::Unscripted>, operation: Operation) -> bool {
    matches!(error, ForgeError::PastDeadline { operation: refused } if *refused == operation)
}

#[test]
fn a_deadline_already_reached_refuses_before_anything_is_sent() {
    let recorded = Recorded::default();
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_or_create_check(
        &head(),
        &verdict(),
        by(&pending(), deadline_after(Duration::ZERO)),
    );

    let error = found.expect_err("the deadline has come");
    assert!(past(&error, Operation::ListChecks), "{error:?}");
    assert_eq!(recorded.sent(), Vec::new());
}

#[test]
fn a_request_that_could_outlast_the_deadline_by_its_own_timeout_is_not_sent() {
    let recorded = Recorded::default();
    let waits = Waits::default();

    let written = github(&recorded, &waits).update_check_if(
        check(),
        "pending:39168909542:3",
        by(&pending(), deadline_after(SEND_TIMEOUT)),
    );

    assert!(
        matches!(&written, Err(CompareAndSetError::Forge(error)) if past(error, Operation::ReadCheck)),
        "ending exactly at the deadline is past it: {written:?}"
    );
    assert_eq!(recorded.sent(), Vec::new());
}

#[test]
fn a_retry_after_that_would_pass_the_deadline_is_neither_waited_for_nor_sent() {
    // GitHub asks for 60 s, which the issue's 2-minute TTL cannot absorb per retry.
    let recorded = Recorded::default().then(
        Method::Get,
        &path("check-runs/117798470001"),
        reply(403, &[("Retry-After", "60")], Vec::new()),
    );
    let waits = Waits::default();

    let written = github(&recorded, &waits).update_check_if(
        check(),
        "pending:39168909542:3",
        by(&pending(), deadline_after(Duration::from_secs(60))),
    );

    assert!(
        matches!(&written, Err(CompareAndSetError::Forge(error)) if past(error, Operation::ReadCheck)),
        "{written:?}"
    );
    assert_eq!(waits.taken(), Vec::new(), "no wait taken");
    assert_eq!(recorded.sent().len(), 1, "only the first read");
    assert_eq!(recorded.writes(), Vec::new(), "no PATCH");
}

#[test]
fn retries_stop_at_the_deadline_and_no_request_is_sent_after_it() {
    // Sends at 0 s and 1 s fit with their 30 s timeout before 31.5 s; the 2 s wait would not.
    let recorded = (0..4).fold(Recorded::default(), |recorded, _| {
        recorded.then(Method::Get, &listing(), reply(502, &[], Vec::new()))
    });
    let waits = Waits::default();
    let deadline = deadline_after(SEND_TIMEOUT + Duration::from_millis(1_500));

    let found = github(&recorded, &waits).find_or_create_check(
        &head(),
        &verdict(),
        by(&pending(), deadline),
    );

    let error = found.expect_err("the deadline stops the retries");
    assert!(past(&error, Operation::ListChecks), "{error:?}");
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
    assert_eq!(recorded.sent().len(), 2);
    assert_eq!(
        recorded.unused(),
        2,
        "the last two attempts were never made"
    );
}

#[test]
fn a_create_whose_retry_would_pass_the_deadline_is_neither_probed_nor_posted_again() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &listing(),
            fixture("check_runs_foreign_only.json"),
        )
        .then(
            Method::Post,
            &path("check-runs"),
            reply(502, &[], Vec::new()),
        );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_or_create_check(
        &head(),
        &verdict(),
        by(
            &pending(),
            deadline_after(SEND_TIMEOUT + Duration::from_millis(500)),
        ),
    );

    let error = found.expect_err("the retry would pass the deadline");
    assert!(past(&error, Operation::CreateCheck), "{error:?}");
    assert!(error.to_string().starts_with("publish-error: "), "{error}");
    assert_eq!(waits.taken(), Vec::new());
    assert_eq!(recorded.writes().len(), 1, "one POST, never repeated");
    assert_eq!(recorded.unused(), 0);
}

#[test]
fn a_far_deadline_changes_nothing() {
    let recorded = Recorded::default()
        .then(Method::Get, &listing(), reply(502, &[], Vec::new()))
        .ok(
            Method::Get,
            &listing(),
            fixture("check_runs_same_name.json"),
        );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_or_create_check(
        &head(),
        &verdict(),
        by(&pending(), deadline_after(Duration::from_secs(3_600))),
    );

    assert!(found.is_ok(), "{found:?}");
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
}
