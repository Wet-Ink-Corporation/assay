//! The per-repo reconcile's port over recorded responses (06 §4). `open_pulls_all.json`,
//! `check_runs_reconcile.json` and `head_runs_live.json` were recorded from this repository with
//! `gh api` while PR 270's `rust` run was in progress, trimmed of URL noise; in the check runs,
//! the in-progress `gate` job is re-attributed to the App as its verdict check and the
//! completed `msrv` job as one of its gate checks, since the App has not published yet.

use assay_app::{
    CheckConclusion, CheckRunId, CheckState, CheckStatus, CheckUpdate, CommitId, LiveRun, OpenPull,
    PullHead, ReconcileCheck, ReconcileForge,
};
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::json;

use super::recorded::{
    Recorded, Waits, first_write, fixture, fixture_json, github, id, json, path,
};

/// PR 270's head while its run was live.
const LIVE_HEAD: &str = "00cacbbedc7aca477c6a470abca4e5e394174db6";

fn live_head() -> CommitId {
    LIVE_HEAD.parse().expect("a full commit id")
}

fn checks_path() -> String {
    path(&format!(
        "commits/{LIVE_HEAD}/check-runs?filter=all&per_page=100"
    ))
}

fn at(text: &str) -> assay_app::LedgerTime {
    text.parse().expect("a ledger time")
}

#[test]
fn the_open_pulls_are_listed_with_their_heads() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls?state=open&per_page=100"),
        fixture("open_pulls_all.json"),
    );
    let waits = Waits::default();

    let pulls = github(&recorded, &waits).open_pulls(10);

    let expected: Vec<OpenPull> = [
        (277, "bd0edf7171ec0528e6f29f7ea0e59025a401e9f2"),
        (271, "347cb035794294f93a668216d86258ef4ad13979"),
        (270, LIVE_HEAD),
    ]
    .into_iter()
    .map(|(number, head)| OpenPull {
        number: id(number),
        head_sha: head.parse().expect("a full commit id"),
    })
    .collect();
    assert_eq!(pulls.expect("listed"), expected);
}

#[test]
fn only_the_apps_checks_are_read_with_their_start_time() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &checks_path(),
        fixture("check_runs_reconcile.json"),
    );
    let waits = Waits::default();

    let checks = github(&recorded, &waits).app_checks(&live_head(), 10);

    assert_eq!(
        checks.expect("listed"),
        [
            ReconcileCheck {
                id: CheckRunId(id(118_142_583_473)),
                name: "assay / gate: temper".to_owned(),
                external_id: Some("9d92d7e9-b4ef-5e81-9bd7-bf80bcf43df5".to_owned()),
                status: CheckStatus::Completed,
                started_at: at("2026-10-19T15:17:23.000Z"),
            },
            ReconcileCheck {
                id: CheckRunId(id(118_142_583_132)),
                name: "assay / verdict".to_owned(),
                external_id: Some("pending:39276656667:1".to_owned()),
                status: CheckStatus::InProgress,
                started_at: at("2026-10-19T15:17:24.000Z"),
            },
        ]
    );
}

#[test]
fn a_start_time_in_another_form_is_refused() {
    let mut listing = fixture_json("check_runs_reconcile.json");
    listing["check_runs"][4]["started_at"] = json!("2026-10-19T15:17:24.5Z");
    let recorded = Recorded::default().ok(Method::Get, &checks_path(), json(&listing));
    let waits = Waits::default();

    let checks = github(&recorded, &waits).app_checks(&live_head(), 10);

    assert!(
        matches!(
            checks,
            Err(ForgeError::Malformed { operation: Operation::ListChecks, detail })
                if detail == "a time is not YYYY-MM-DDTHH:MM:SSZ"
        ),
        "{checks:?}"
    );
}

#[test]
fn the_live_runs_on_a_head_are_its_runs_not_completed() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path(&format!("actions/runs?head_sha={LIVE_HEAD}&per_page=100")),
        fixture("head_runs_live.json"),
    );
    let waits = Waits::default();

    let live = github(&recorded, &waits).live_runs(&live_head(), 10);

    assert_eq!(
        live.expect("listed"),
        [LiveRun {
            path: ".github/workflows/rust.yml".to_owned(),
        }]
    );
}

#[test]
fn a_completed_run_is_not_live() {
    let mut runs = fixture_json("head_runs_live.json");
    runs["workflow_runs"][0]["status"] = json!("completed");
    let recorded = Recorded::default().ok(
        Method::Get,
        &path(&format!("actions/runs?head_sha={LIVE_HEAD}&per_page=100")),
        json(&runs),
    );
    let waits = Waits::default();

    let live = github(&recorded, &waits).live_runs(&live_head(), 10);

    assert_eq!(live.expect("listed"), []);
}

#[test]
fn a_closed_pull_reads_closed() {
    let recorded = Recorded::default().ok(Method::Get, &path("pulls/268"), fixture("pull.json"));
    let waits = Waits::default();

    let now = github(&recorded, &waits).pull_head(id(268));

    assert_eq!(now.expect("read"), PullHead::Closed);
}

#[test]
fn a_superseded_check_is_written_cancelled_by_compare_and_set() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("check-runs/117798470001"),
            fixture("check_run_app.json"),
        )
        .ok(Method::Patch, &path("check-runs/117798470001"), Vec::new());
    let waits = Waits::default();
    let update = CheckUpdate {
        state: CheckState::Completed(CheckConclusion::Cancelled),
        title: "assay: cancelled (superseded)".to_owned(),
        summary: "Superseded.".to_owned(),
        text: String::new(),
        external_id: "reconcile:5000".to_owned(),
    };

    let written = ReconcileForge::update_check_if(
        &github(&recorded, &waits),
        CheckRunId(id(117_798_470_001)),
        "pending:39168909542:3",
        &update,
    );

    assert!(written.is_ok(), "{written:?}");
    assert_eq!(
        first_write(&recorded),
        Some(json!({
            "status": "completed",
            "conclusion": "cancelled",
            "external_id": "reconcile:5000",
            "output": {"title": "assay: cancelled (superseded)", "summary": "Superseded.", "text": ""},
        }))
    );
}
