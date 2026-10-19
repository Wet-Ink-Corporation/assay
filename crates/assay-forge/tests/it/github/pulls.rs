//! `decide`'s view of the PR now, over recorded responses.

use assay_app::{CurrentPullRequest, PullOnHead, PullRequestState};
use assay_domain::RunUid;
use assay_forge::github::{ForgeError, Method};

use super::recorded::{
    HEAD, Recorded, Waits, fixture, fixture_json, github, head, id, json, path, pr,
};

/// The recorded run, `rust` run 218 on [`HEAD`].
fn run() -> RunUid {
    RunUid {
        repo_id: id(1_413_773_798),
        run_id: id(39_168_909_542),
        run_attempt: id(1),
    }
}

/// The run listing of the recorded run's workflow on [`HEAD`].
fn workflow_runs() -> String {
    path(&format!(
        "actions/workflows/381056875/runs?head_sha={HEAD}&per_page=100"
    ))
}

#[test]
fn the_current_head_and_base_are_read() {
    let recorded = Recorded::default().ok(Method::Get, &path("pulls/268"), fixture("pull.json"));
    let waits = Waits::default();

    let current = github(&recorded, &waits).current(pr());

    assert_eq!(
        current.expect("read"),
        CurrentPullRequest {
            head_sha: head(),
            base_ref: "main".to_owned(),
        }
    );
}

#[test]
fn every_pull_on_the_head_is_listed_with_its_state_and_base() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path(&format!("commits/{HEAD}/pulls?per_page=100")),
        fixture("commit_pulls.json"),
    );
    let waits = Waits::default();

    let pulls = github(&recorded, &waits).pulls_on_head(&head());

    assert_eq!(
        pulls.expect("listed"),
        [
            PullOnHead {
                number: pr(),
                open: false,
                base_ref: "main".to_owned(),
            },
            PullOnHead {
                number: id(269),
                open: true,
                base_ref: "release/1.x".to_owned(),
            },
        ]
    );
}

#[test]
fn a_higher_run_number_of_the_workflow_is_a_newer_run() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("actions/runs/39168909542"),
            fixture("run.json"),
        )
        .ok(Method::Get, &workflow_runs(), fixture("workflow_runs.json"));
    let waits = Waits::default();

    let newer = github(&recorded, &waits).newer_run(run(), &head());

    assert!(newer.expect("listed"), "run 219 is newer than 218");
}

#[test]
fn the_run_alone_is_not_newer_than_itself() {
    let mut alone = fixture_json("workflow_runs.json");
    if let Some(runs) = alone["workflow_runs"].as_array_mut() {
        runs.truncate(1);
    }
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("actions/runs/39168909542"),
            fixture("run.json"),
        )
        .ok(Method::Get, &workflow_runs(), json(&alone));
    let waits = Waits::default();

    let newer = github(&recorded, &waits).newer_run(run(), &head());

    assert!(!newer.expect("listed"), "only run 218");
}

#[test]
fn a_listed_run_on_another_head_is_refused() {
    let mut moved = fixture_json("workflow_runs.json");
    moved["workflow_runs"][1]["head_sha"] =
        serde_json::json!("ca42d86e668fbb18dab3ea4255fcd09518412f3b");
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("actions/runs/39168909542"),
            fixture("run.json"),
        )
        .ok(Method::Get, &workflow_runs(), json(&moved));
    let waits = Waits::default();

    let newer = github(&recorded, &waits).newer_run(run(), &head());

    assert!(
        matches!(newer, Err(ForgeError::Malformed { .. })),
        "{newer:?}"
    );
}
