//! The App's summary comment by hidden marker, written only when it changed, and the App's
//! inline review comments; nobody else's comment ever counts, whatever it contains.

use std::num::NonZeroU64;
use std::time::Duration;

use assay_app::{
    CommentForge, CommentId, Forge, InlineComment, SUMMARY_MARKER, SummaryComment, Upserted,
    VerdictForge,
};
use assay_domain::Side;
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::json;

use super::recorded::{
    Recorded, Waits, first_write, fixture, fixture_json, github, head, id, json, path, pr, reply,
    write_bodies,
};

/// PR 268's issue comments.
fn listing() -> String {
    path("issues/268/comments?per_page=100")
}

/// The App's recorded summary.
fn app_summary() -> SummaryComment {
    SummaryComment {
        id: CommentId(id(6_255_521_954)),
        body: format!("{SUMMARY_MARKER}\n### assay: pending"),
    }
}

#[test]
fn the_marker_finds_only_the_apps_comment() {
    let recorded = Recorded::default().ok(Method::Get, &listing(), fixture("issue_comments.json"));
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_summary(pr(), SUMMARY_MARKER);

    assert_eq!(found.expect("listed"), Some(app_summary()));
}

#[test]
fn the_bot_user_without_the_app_is_not_the_app() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &listing(),
        fixture("issue_comments_bot_without_app.json"),
    );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_summary(pr(), SUMMARY_MARKER);

    assert_eq!(found.expect("listed"), None);
}

#[test]
fn two_app_summaries_are_refused() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &listing(),
        fixture("issue_comments_two_app.json"),
    );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_summary(pr(), SUMMARY_MARKER);

    assert!(
        matches!(
            found,
            Err(ForgeError::Ambiguous {
                operation: Operation::ListComments
            })
        ),
        "{found:?}"
    );
}

#[test]
fn an_unchanged_summary_is_not_written() {
    let recorded = Recorded::default().ok(Method::Get, &listing(), fixture("issue_comments.json"));
    let waits = Waits::default();

    let upserted =
        github(&recorded, &waits).upsert_comment(pr(), SUMMARY_MARKER, &app_summary().body);

    assert_eq!(upserted.expect("read"), Upserted::Unchanged);
    assert_eq!(recorded.writes(), Vec::new(), "write-if-changed");
}

#[test]
fn a_changed_summary_is_edited_in_place() {
    let recorded = Recorded::default()
        .ok(Method::Get, &listing(), fixture("issue_comments.json"))
        .ok(
            Method::Patch,
            &path("issues/comments/6255521954"),
            json(&json!({})),
        );
    let waits = Waits::default();
    let body = format!("{SUMMARY_MARKER}\n### assay: success");

    let upserted = github(&recorded, &waits).upsert_comment(pr(), SUMMARY_MARKER, &body);

    assert_eq!(upserted.expect("edited"), Upserted::Updated);
    assert_eq!(first_write(&recorded), Some(json!({ "body": body })));
}

#[test]
fn a_forged_marker_does_not_stop_the_app_posting_its_own() {
    let mut forged_only = fixture_json("issue_comments.json");
    if let Some(comments) = forged_only.as_array_mut() {
        comments.truncate(2);
    }
    let recorded = Recorded::default()
        .ok(Method::Get, &listing(), json(&forged_only))
        .then(
            Method::Post,
            &path("issues/268/comments"),
            reply(201, &[], json(&json!({}))),
        );
    let waits = Waits::default();

    let upserted =
        github(&recorded, &waits).upsert_comment(pr(), SUMMARY_MARKER, &app_summary().body);

    assert_eq!(upserted.expect("posted"), Upserted::Created);
    assert_eq!(
        first_write(&recorded),
        Some(json!({ "body": app_summary().body }))
    );
}

#[test]
fn a_summary_that_landed_before_a_5xx_is_not_posted_again() {
    let recorded = Recorded::default()
        .then(
            Method::Post,
            &path("issues/268/comments"),
            reply(504, &[], Vec::new()),
        )
        .ok(Method::Get, &listing(), fixture("issue_comments.json"));
    let waits = Waits::default();

    let created = github(&recorded, &waits).create_summary(pr(), &app_summary().body);

    assert_eq!(created.expect("landed"), ());
    assert_eq!(recorded.writes().len(), 1, "one POST only");
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
}

/// A summary body the recorded comments do not hold.
fn new_summary() -> String {
    format!("{SUMMARY_MARKER}\n### assay: success")
}

#[test]
fn a_summary_the_probe_does_not_find_is_posted_again() {
    let post = path("issues/268/comments");
    let recorded = Recorded::default()
        .then(Method::Post, &post, reply(502, &[], Vec::new()))
        .ok(Method::Get, &listing(), fixture("issue_comments.json"))
        .then(Method::Post, &post, reply(201, &[], json(&json!({}))));
    let waits = Waits::default();

    let created = github(&recorded, &waits).create_summary(pr(), &new_summary());

    assert_eq!(created.expect("the second POST lands"), ());
    let body = Some(json!({ "body": new_summary() }));
    assert_eq!(write_bodies(&recorded), [body.clone(), body]);
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
    assert_eq!(recorded.unused(), 0, "POST, probe, POST");
}

#[test]
fn a_summary_failing_four_times_with_probes_is_a_publish_error() {
    let post = path("issues/268/comments");
    let mut recorded = Recorded::default().then(Method::Post, &post, reply(502, &[], Vec::new()));
    for _ in 0..3 {
        recorded = recorded
            .ok(Method::Get, &listing(), fixture("issue_comments.json"))
            .then(Method::Post, &post, reply(502, &[], Vec::new()));
    }
    let waits = Waits::default();

    let created = github(&recorded, &waits).create_summary(pr(), &new_summary());

    assert!(
        matches!(
            created,
            Err(ForgeError::Exhausted {
                operation: Operation::CreateComment,
                status: 502,
                attempts: 4
            })
        ),
        "{created:?}"
    );
    assert_eq!(write_bodies(&recorded).len(), 4, "four POSTs");
    assert_eq!(
        waits.taken(),
        [1, 2, 4].map(Duration::from_secs),
        "three backoffs"
    );
}

#[test]
fn an_inline_comment_the_probe_does_not_find_is_posted_again() {
    let post = path("pulls/268/comments");
    let recorded = Recorded::default()
        .then(Method::Post, &post, reply(502, &[], Vec::new()))
        .ok(
            Method::Get,
            &path("pulls/268/comments?per_page=100"),
            fixture("review_comments.json"),
        )
        .then(Method::Post, &post, reply(201, &[], json(&json!({}))));
    let waits = Waits::default();
    let commit = head();
    let comment = InlineComment {
        commit_id: &commit,
        path: "crates/assay-forge/src/github.rs",
        line: id(14),
        start_line: None,
        side: Side::Right,
        body: "new finding".to_owned(),
    };

    let created = github(&recorded, &waits).create_review_comment(pr(), &comment);

    assert_eq!(created.expect("the second POST lands"), ());
    let body = Some(json!({
        "body": "new finding",
        "commit_id": commit.as_str(),
        "path": "crates/assay-forge/src/github.rs",
        "line": 14,
        "side": "RIGHT",
    }));
    assert_eq!(write_bodies(&recorded), [body.clone(), body]);
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
}

#[test]
fn an_empty_marker_is_refused_unasked() {
    let recorded = Recorded::default();
    let waits = Waits::default();
    let forge = github(&recorded, &waits);

    let found = forge.find_summary(pr(), "");
    let upserted = forge.upsert_comment(pr(), "", "body");

    assert!(
        matches!(found, Err(ForgeError::InvalidRequest { detail, .. }) if detail == "the marker is empty"),
        "{found:?}"
    );
    assert!(
        matches!(upserted, Err(ForgeError::InvalidRequest { detail, .. }) if detail == "the marker is empty"),
        "{upserted:?}"
    );
    assert_eq!(recorded.sent(), Vec::new(), "nothing sent");
}

#[test]
fn a_body_without_its_marker_is_refused_unasked() {
    let recorded = Recorded::default();
    let waits = Waits::default();

    let upserted =
        github(&recorded, &waits).upsert_comment(pr(), SUMMARY_MARKER, "### assay: success");

    assert!(
        matches!(upserted, Err(ForgeError::InvalidRequest { detail, .. }) if detail == "the body does not carry the marker"),
        "{upserted:?}"
    );
    assert_eq!(recorded.sent(), Vec::new(), "nothing sent");
}

#[test]
fn only_the_apps_review_comments_are_returned() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls/268/comments?per_page=100"),
        fixture("review_comments.json"),
    );
    let waits = Waits::default();

    let bodies = github(&recorded, &waits).app_review_comments(pr());

    assert_eq!(
        bodies.expect("listed"),
        ["<!-- assay-fp:0123456789abcdef -->\n**major** finding"]
    );
}

#[test]
fn an_inline_comment_over_a_span_names_both_ends_and_one_side() {
    let recorded = Recorded::default().then(
        Method::Post,
        &path("pulls/268/comments"),
        reply(201, &[], json(&json!({}))),
    );
    let waits = Waits::default();
    let commit = head();
    let comment = InlineComment {
        commit_id: &commit,
        path: "crates/assay-forge/src/github.rs",
        line: id(14),
        start_line: NonZeroU64::new(12),
        side: Side::Left,
        body: "finding".to_owned(),
    };

    github(&recorded, &waits)
        .create_review_comment(pr(), &comment)
        .expect("posted");

    assert_eq!(
        first_write(&recorded),
        Some(json!({
            "body": "finding",
            "commit_id": commit.as_str(),
            "path": "crates/assay-forge/src/github.rs",
            "line": 14,
            "side": "LEFT",
            "start_line": 12,
            "start_side": "LEFT",
        }))
    );
}
