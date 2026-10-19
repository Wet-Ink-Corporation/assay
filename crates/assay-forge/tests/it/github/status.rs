//! `assay status`'s read port over recorded responses.

use std::num::NonZeroU64;

use assay_app::{
    AppId, CheckStatus, CheckView, ObservedConclusion, PullView, ReviewCommentView, StatusForge,
    Viewer,
};
use assay_forge::github::{ForgeError, Method};
use serde_json::json;

use super::recorded::{
    HEAD, Recorded, Waits, app, fixture, fixture_json, github, head, id, json as json_body, owner,
    path, pr, reply,
};

/// PR 268 as `status` sees it.
fn pull_268() -> PullView {
    PullView {
        number: pr(),
        head_sha: head(),
        head_ref: "claude/project-thread-t0rwlw".to_owned(),
        base_ref: "main".to_owned(),
    }
}

#[test]
fn a_pull_is_read_and_a_missing_one_is_none() {
    let recorded = Recorded::default()
        .ok(Method::Get, &path("pulls/268"), fixture("pull.json"))
        .then(
            Method::Get,
            &path("pulls/9999"),
            reply(404, &[], Vec::new()),
        );
    let waits = Waits::default();
    let forge = github(&recorded, &waits);

    assert_eq!(forge.pull(pr()).expect("read"), Some(pull_268()));
    assert_eq!(forge.pull(id(9999)).expect("read"), None);
}

#[test]
fn open_pulls_keep_heads_of_this_repository_only() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path(
            "pulls?state=open&head=Wet-Ink-Corporation:claude%2Fproject-thread-t0rwlw&per_page=100",
        ),
        fixture("open_pulls.json"),
    );
    let waits = Waits::default();

    let pulls = github(&recorded, &waits).open_pulls("claude/project-thread-t0rwlw", 2);

    assert_eq!(
        pulls.expect("listed"),
        [pull_268()],
        "the fork's PR is not ours"
    );
}

#[test]
fn check_runs_of_every_app_are_read_up_to_the_limit() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path(&format!(
            "commits/{HEAD}/check-runs?filter=all&per_page=100"
        )),
        fixture("check_runs_on_head.json"),
    );
    let waits = Waits::default();

    let checks = github(&recorded, &waits).check_runs(&head(), 1);

    assert_eq!(
        checks.expect("listed"),
        [CheckView {
            app: AppId(id(15_368)),
            name: "report-comment".to_owned(),
            status: CheckStatus::Completed,
            conclusion: Some(ObservedConclusion::Success),
            external_id: Some("d2defc3b-9472-58da-bec7-4caf3d4b8da8".to_owned()),
            output_title: None,
            output_text: None,
            url: "https://github.com/Wet-Ink-Corporation/assay/actions/runs/39168909542/job/117798464273"
                .to_owned(),
        }]
    );
}

#[test]
fn another_apps_pre_start_state_reads_as_queued_and_the_apps_is_refused() {
    let listing = path(&format!(
        "commits/{HEAD}/check-runs?filter=all&per_page=100"
    ));
    let mut waiting = fixture_json("check_runs_same_name.json");
    for check in waiting["check_runs"].as_array_mut().into_iter().flatten() {
        check["status"] = json!("waiting");
        check["conclusion"] = json!(null);
    }
    let foreign_only = json!({ "total_count": 1, "check_runs": [waiting["check_runs"][0]] });
    let recorded = Recorded::default()
        .ok(Method::Get, &listing, json_body(&foreign_only))
        .ok(Method::Get, &listing, json_body(&waiting));
    let waits = Waits::default();
    let forge = github(&recorded, &waits);

    let foreign = forge.check_runs(&head(), 10).expect("listed");
    let with_app = forge.check_runs(&head(), 10);

    assert_eq!(
        foreign.iter().map(|check| check.status).collect::<Vec<_>>(),
        [CheckStatus::Queued]
    );
    assert!(
        matches!(with_app, Err(ForgeError::Malformed { detail, .. }) if detail == "unknown check status"),
        "{with_app:?}"
    );
}

#[test]
fn review_comments_are_the_apps_only() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls/268/comments?per_page=100"),
        fixture("review_comments.json"),
    );
    let waits = Waits::default();

    let comments = github(&recorded, &waits).review_comments(pr(), app(), 10);

    assert_eq!(
        comments.expect("listed"),
        [ReviewCommentView {
            app: Some(app()),
            body: "<!-- assay-fp:0123456789abcdef -->\n**major** finding".to_owned(),
            path: "crates/assay-forge/src/github.rs".to_owned(),
            line: NonZeroU64::new(12),
            url: "https://github.com/Wet-Ink-Corporation/assay/pull/268#discussion_r2400000001"
                .to_owned(),
        }]
    );
}

#[test]
fn review_comments_of_another_app_are_refused_unasked() {
    let recorded = Recorded::default();
    let waits = Waits::default();
    let other = AppId(id(15_368));

    let comments = github(&recorded, &waits).review_comments(pr(), other, 10);

    assert!(
        matches!(comments, Err(ForgeError::UnknownApp { app }) if app == other),
        "{comments:?}"
    );
    assert_eq!(recorded.sent(), Vec::new(), "nothing sent");
}

#[test]
fn the_viewer_is_the_authenticated_user() {
    let recorded = Recorded::default().ok(Method::Get, "user", fixture("viewer.json"));
    let waits = Waits::default();

    let viewer = github(&recorded, &waits).viewer();

    assert_eq!(
        viewer.expect("read"),
        Viewer {
            id: owner(),
            login: "Ryan-Britton".to_owned(),
        }
    );
}
