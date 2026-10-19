//! The per-repo reconcile's port over recorded responses (06 §4). `open_pulls_all.json` and
//! `check_runs_reconcile.json` were recorded from this repository with `gh api` while PR 270's
//! `rust` run was in progress, trimmed of URL noise; in the check runs, the in-progress `gate`
//! job is re-attributed to the App as its verdict check and the completed `msrv` job as one of
//! its gate checks, since the App has not published yet. `run_dispatch_live.json` is a queued
//! run recorded the same way, re-labelled as the in-progress `workflow_dispatch` run of
//! `assay.yml` a requeue starts on the default branch: its `head_sha` is not a PR head.

use assay_app::{
    CheckConclusion, CheckRunId, CheckState, CheckStatus, CheckUpdate, CommitId, OpenPull,
    PullHead, ReconcileCheck, ReconcileForge, RunProgress, StampedRun,
};
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::json;

use super::recorded::{
    Recorded, Unscripted, Waits, first_write, fixture, fixture_json, github, id, json, path, reply,
};

/// PR 270's head while its run was live.
const LIVE_HEAD: &str = "00cacbbedc7aca477c6a470abca4e5e394174db6";

/// The recorded run, as a requeue's `workflow_dispatch` run.
const DISPATCH_RUN: u64 = 39_282_503_375;

fn live_head() -> CommitId {
    LIVE_HEAD.parse().expect("a full commit id")
}

fn checks_path() -> String {
    path(&format!(
        "commits/{LIVE_HEAD}/check-runs?filter=all&per_page=100"
    ))
}

fn run_path() -> String {
    path(&format!("actions/runs/{DISPATCH_RUN}"))
}

fn at(text: &str) -> assay_app::LedgerTime {
    text.parse().expect("a ledger time")
}

/// The dispatch run, with `edit` applied, as the only recorded response.
fn dispatch_run(edit: impl FnOnce(&mut serde_json::Value)) -> Recorded {
    let mut run = fixture_json("run_dispatch_live.json");
    edit(&mut run);
    Recorded::default().ok(Method::Get, &run_path(), json(&run))
}

fn malformed<T>(
    read: &Result<T, ForgeError<Unscripted>>,
    operation: Operation,
    detail: &str,
) -> bool {
    matches!(read, Err(ForgeError::Malformed { operation: found, detail: said })
        if *found == operation && *said == detail)
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
fn a_listed_open_pull_that_is_not_open_is_refused() {
    let mut listing = fixture_json("open_pulls_all.json");
    listing[1]["state"] = json!("closed");
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("pulls?state=open&per_page=100"),
        json(&listing),
    );
    let waits = Waits::default();

    let pulls = github(&recorded, &waits).open_pulls(10);

    assert!(
        malformed(&pulls, Operation::ListPulls, "a listed open PR is not open"),
        "{pulls:?}"
    );
}

#[test]
fn only_the_apps_checks_are_read() {
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
            },
            ReconcileCheck {
                id: CheckRunId(id(118_142_583_132)),
                name: "assay / verdict".to_owned(),
                external_id: Some("pending:39276656667:1".to_owned()),
                status: CheckStatus::InProgress,
            },
        ]
    );
}

#[test]
fn an_app_check_listed_on_another_head_is_refused() {
    let mut listing = fixture_json("check_runs_reconcile.json");
    listing["check_runs"][4]["head_sha"] = json!("347cb035794294f93a668216d86258ef4ad13979");
    let recorded = Recorded::default().ok(Method::Get, &checks_path(), json(&listing));
    let waits = Waits::default();

    let checks = github(&recorded, &waits).app_checks(&live_head(), 10);

    assert!(
        malformed(
            &checks,
            Operation::ListChecks,
            "a listed check is on another head"
        ),
        "{checks:?}"
    );
}

#[test]
fn the_stamped_run_is_read_whatever_commit_it_runs_on() {
    let recorded = dispatch_run(|_| {});
    let waits = Waits::default();

    let run = github(&recorded, &waits).workflow_run(id(DISPATCH_RUN));

    assert_eq!(
        run.expect("read"),
        Some(StampedRun {
            progress: RunProgress::Live,
            started_at: at("2026-10-19T16:03:33.000Z"),
        })
    );
}

#[test]
fn a_run_live_in_any_state_but_completed_reads_live_and_completed_reads_completed() {
    let cases = [
        ("queued", RunProgress::Live),
        ("waiting", RunProgress::Live),
        ("pending", RunProgress::Live),
        ("requested", RunProgress::Live),
        ("completed", RunProgress::Completed),
    ];
    for (status, progress) in cases {
        let recorded = dispatch_run(|run| run["status"] = json!(status));
        let waits = Waits::default();

        let run = github(&recorded, &waits).workflow_run(id(DISPATCH_RUN));

        assert_eq!(
            run.expect("read").map(|run| run.progress),
            Some(progress),
            "{status}"
        );
    }
}

#[test]
fn a_run_github_does_not_have_reads_none() {
    let recorded = Recorded::default().then(
        Method::Get,
        &run_path(),
        reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
    );
    let waits = Waits::default();

    let run = github(&recorded, &waits).workflow_run(id(DISPATCH_RUN));

    assert_eq!(run.expect("a missing run is not an error"), None);
}

/// A case: its name, the edit made to the recorded run, and the refusal's detail.
type Refusal = (&'static str, fn(&mut serde_json::Value), &'static str);

#[test]
fn a_run_that_is_not_the_one_asked_for_or_has_no_readable_start_is_refused() {
    let cases: [Refusal; 4] = [
        (
            "another run",
            |run| run["id"] = json!(39_168_909_542_u64),
            "GitHub returned another run",
        ),
        (
            "no run_started_at",
            |run| {
                run.as_object_mut()
                    .expect("a run is an object")
                    .remove("run_started_at");
            },
            "a run has no run_started_at",
        ),
        (
            "a start with a fraction",
            |run| run["run_started_at"] = json!("2026-10-19T16:03:33.5Z"),
            "a time is not YYYY-MM-DDTHH:MM:SSZ",
        ),
        (
            "an unknown status",
            |run| run["status"] = json!("paused"),
            "a run's status is not one GitHub documents",
        ),
    ];
    for (case, edit, detail) in cases {
        let recorded = dispatch_run(edit);
        let waits = Waits::default();

        let run = github(&recorded, &waits).workflow_run(id(DISPATCH_RUN));

        assert!(
            malformed(&run, Operation::ListRuns, detail),
            "{case}: {run:?}"
        );
    }
}

#[test]
fn an_open_pull_reads_its_head() {
    let mut pull = fixture_json("pull.json");
    pull["state"] = json!("open");
    let recorded = Recorded::default().ok(Method::Get, &path("pulls/268"), json(&pull));
    let waits = Waits::default();

    let now = github(&recorded, &waits).pull_head(id(268));

    assert_eq!(
        now.expect("read"),
        PullHead::Open(
            "70aa6d748be6f3884e85d294112f9addf83cee59"
                .parse()
                .expect("a full commit id")
        )
    );
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
