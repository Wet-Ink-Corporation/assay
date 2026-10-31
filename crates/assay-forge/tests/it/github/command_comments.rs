//! `CommandComments` over recorded responses: the comment a command record names, re-read by
//! id. `issue_comment.json` is comment 6255521952 on PR 268 (posted through the
//! `github-actions` app), recorded with `gh api` and trimmed of URL noise.

use assay_app::{CommandComments, CommentId, IssueComment, MAX_COMMENT_BYTES, UserId};
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::json;

use super::recorded::{
    Recorded, Unscripted, Waits, fixture, fixture_json, github, id, json, path, reply,
};

/// The recorded comment.
const COMMENT: u64 = 6_255_521_952;

fn comment_path() -> String {
    path(&format!("issues/comments/{COMMENT}"))
}

/// The recorded comment with `edit` applied, as the only recorded response.
fn edited(edit: impl FnOnce(&mut serde_json::Value)) -> Recorded {
    let mut comment = fixture_json("issue_comment.json");
    edit(&mut comment);
    Recorded::default().ok(Method::Get, &comment_path(), json(&comment))
}

#[test]
fn a_comment_is_read_with_its_app() {
    let recorded =
        Recorded::default().ok(Method::Get, &comment_path(), fixture("issue_comment.json"));
    let waits = Waits::default();

    let read = github(&recorded, &waits).comment(CommentId(id(COMMENT)));

    let body = fixture_json("issue_comment.json")["body"]
        .as_str()
        .expect("a body")
        .to_owned();
    assert_eq!(
        read.expect("read"),
        Some(IssueComment {
            id: CommentId(id(COMMENT)),
            user: Some(UserId(id(41_898_282))),
            created_at: "2026-10-18T21:44:44Z".parse().expect("a timestamp"),
            updated_at: "2026-10-18T21:44:44Z".parse().expect("a timestamp"),
            body,
            via_app: true,
        })
    );
}

#[test]
fn a_comment_no_app_performed_is_not_via_an_app() {
    let recorded = edited(|comment| {
        comment["performed_via_github_app"] = json!(null);
        comment["updated_at"] = json!("2026-10-18T21:50:01Z");
    });
    let waits = Waits::default();

    let read = github(&recorded, &waits)
        .comment(CommentId(id(COMMENT)))
        .expect("read")
        .expect("present");

    assert_eq!(
        (read.via_app, read.updated_at),
        (false, "2026-10-18T21:50:01Z".parse().expect("a timestamp"))
    );
}

#[test]
fn a_comment_that_omits_performed_via_github_app_is_refused() {
    let recorded = edited(|comment| {
        comment
            .as_object_mut()
            .expect("an object")
            .remove("performed_via_github_app");
    });
    let waits = Waits::default();

    let read = github(&recorded, &waits).comment(CommentId(id(COMMENT)));

    assert!(
        matches!(
            read,
            Err(ForgeError::Malformed {
                operation: Operation::ReadComment,
                detail: "a comment does not say whether an app posted it",
            })
        ),
        "{read:?}"
    );
}

#[test]
fn a_deleted_comment_is_none() {
    let recorded = Recorded::default().then(
        Method::Get,
        &comment_path(),
        reply(404, &[], fixture("not_found.json")),
    );
    let waits = Waits::default();

    let read = github(&recorded, &waits).comment(CommentId(id(COMMENT)));

    assert_eq!(read.expect("read"), None);
}

/// The comment's body read back when it is `size` bytes long.
fn body_of_size(size: usize) -> Result<Option<IssueComment>, ForgeError<Unscripted>> {
    let recorded = edited(|comment| comment["body"] = json!("x".repeat(size)));
    let waits = Waits::default();
    github(&recorded, &waits).comment(CommentId(id(COMMENT)))
}

#[test]
fn a_body_at_the_cap_is_read() {
    let read = body_of_size(MAX_COMMENT_BYTES);

    assert_eq!(
        read.expect("read").map(|comment| comment.body.len()),
        Some(MAX_COMMENT_BYTES)
    );
}

#[test]
fn a_body_past_the_cap_is_refused() {
    let read = body_of_size(MAX_COMMENT_BYTES + 1);

    assert!(
        matches!(
            read,
            Err(ForgeError::Malformed {
                operation: Operation::ReadComment,
                detail: "a comment body is over MAX_COMMENT_BYTES",
            })
        ),
        "{read:?}"
    );
}

/// A change to the recorded comment, for a table of refusals.
type Edit = fn(&mut serde_json::Value);

#[test]
fn a_comment_that_cannot_be_read_exactly_is_refused() {
    let cases: [(Edit, &str); 3] = [
        (
            |comment| comment["id"] = json!(COMMENT + 1),
            "GitHub returned another comment",
        ),
        (
            |comment| comment["body"] = json!(null),
            "a comment has no body",
        ),
        (
            |comment| comment["created_at"] = json!("2026-10-18 21:44:44"),
            "a comment's time is not an RFC 3339 UTC date-time",
        ),
    ];
    for (edit, expected) in cases {
        let recorded = edited(edit);
        let waits = Waits::default();

        let read = github(&recorded, &waits).comment(CommentId(id(COMMENT)));

        assert!(
            matches!(read, Err(ForgeError::Malformed { operation: Operation::ReadComment, detail })
                if detail == expected),
            "{expected}: {read:?}"
        );
    }
}
