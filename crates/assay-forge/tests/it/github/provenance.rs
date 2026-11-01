//! The run provenance port over recorded responses (J4). `run_pull_request_target.json` was
//! recorded from this repository with `gh api actions/runs/41260608626` (PR 352's `rust` run,
//! a same-repository `pull_request` run), trimmed of URL noise and of `head_commit`, and
//! re-labelled as the pipeline's `pull_request_target` run of `assay.yml`, which this
//! repository has not installed yet: `name`, `display_title`, `path` and `event` are the only
//! edits.

use assay_app::{RunEvent, RunProvenance, WorkflowRunFacts};
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::{Value, json};

use super::recorded::{Recorded, Unscripted, Waits, fixture_json, github, id, json, path, reply};

/// The recorded run.
const RUN: u64 = 41_260_608_626;
/// This repository's id, as the recording gives it.
const REPO: u64 = 1_413_773_798;

fn run_path() -> String {
    path(&format!("actions/runs/{RUN}"))
}

/// The recorded run, with `edit` applied, as the only recorded response.
fn recorded_run(edit: impl FnOnce(&mut Value)) -> Recorded {
    let mut run = fixture_json("run_pull_request_target.json");
    edit(&mut run);
    Recorded::default().ok(Method::Get, &run_path(), json(&run))
}

/// What the adapter reads of the recorded run, edited by `edit`.
fn read(edit: impl FnOnce(&mut Value)) -> Result<Option<WorkflowRunFacts>, ForgeError<Unscripted>> {
    let recorded = recorded_run(edit);
    let waits = Waits::default();
    github(&recorded, &waits).workflow_run(id(RUN))
}

/// The recorded run's facts.
fn recorded_facts() -> WorkflowRunFacts {
    WorkflowRunFacts {
        run_id: id(RUN),
        repo_id: id(REPO),
        path: ".github/workflows/assay.yml".to_owned(),
        event: RunEvent::PullRequestTarget,
        head_branch: "claude/assay-60-st8-gates-lint".to_owned(),
        head_repo: Some(id(REPO)),
        run_attempt: id(1),
    }
}

#[test]
fn the_recorded_run_reads_as_its_facts() {
    let recorded = recorded_run(|_| {});
    let waits = Waits::default();

    let run = github(&recorded, &waits).workflow_run(id(RUN));

    assert_eq!(run.expect("read"), Some(recorded_facts()));
    let sent = recorded.sent();
    assert_eq!(
        sent.iter()
            .map(|sent| (sent.method, sent.path.as_str()))
            .collect::<Vec<_>>(),
        [(Method::Get, run_path().as_str())]
    );
}

#[test]
fn a_dispatch_on_the_default_branch_reads_its_event_and_branch() {
    let run = read(|run| {
        run["event"] = json!("workflow_dispatch");
        run["head_branch"] = json!("main");
    });

    assert_eq!(
        run.expect("read"),
        Some(WorkflowRunFacts {
            event: RunEvent::WorkflowDispatch,
            head_branch: "main".to_owned(),
            ..recorded_facts()
        })
    );
}

#[test]
fn each_event_assay_runs_for_is_read_and_any_other_is_other() {
    let cases = [
        ("pull_request_target", RunEvent::PullRequestTarget),
        ("workflow_dispatch", RunEvent::WorkflowDispatch),
        ("issue_comment", RunEvent::IssueComment),
        ("schedule", RunEvent::Schedule),
        ("pull_request", RunEvent::Other),
        ("push", RunEvent::Other),
        ("dynamic", RunEvent::Other),
        ("Pull_Request_Target", RunEvent::Other),
    ];
    for (name, event) in cases {
        let run = read(|run| run["event"] = json!(name));

        assert_eq!(
            run.expect("read").map(|facts| facts.event),
            Some(event),
            "{name}"
        );
    }
}

#[test]
fn a_fork_s_or_a_deleted_head_repository_is_read_as_reported() {
    let fork = read(|run| run["head_repository"]["id"] = json!(REPO + 1));
    let deleted = read(|run| run["head_repository"] = Value::Null);

    assert_eq!(
        fork.expect("read").map(|facts| facts.head_repo),
        Some(Some(id(REPO + 1)))
    );
    assert_eq!(
        deleted.expect("read").map(|facts| facts.head_repo),
        Some(None)
    );
}

#[test]
fn a_re_run_reads_its_attempt() {
    let run = read(|run| run["run_attempt"] = json!(2));

    assert_eq!(
        run.expect("read").map(|facts| facts.run_attempt),
        Some(id(2))
    );
}

#[test]
fn a_run_github_does_not_have_reads_none() {
    let recorded = Recorded::default().then(
        Method::Get,
        &run_path(),
        reply(404, &[], br#"{"message":"Not Found"}"#.to_vec()),
    );
    let waits = Waits::default();

    let run = github(&recorded, &waits).workflow_run(id(RUN));

    assert_eq!(run.expect("a missing run is not an error"), None);
}

/// A case: its name, the edit made to the recorded run, and the refusal's detail.
type Refusal = (&'static str, fn(&mut Value), &'static str);

#[test]
fn an_answer_provenance_cannot_judge_is_refused() {
    let cases: [Refusal; 5] = [
        (
            "another run",
            |run| run["id"] = json!(RUN + 1),
            "GitHub returned another run",
        ),
        (
            "no head branch",
            |run| run["head_branch"] = Value::Null,
            "a run has no head branch",
        ),
        (
            "a zero repository",
            |run| run["repository"]["id"] = json!(0),
            "a repository id is zero",
        ),
        (
            "a zero head repository",
            |run| run["head_repository"]["id"] = json!(0),
            "a repository id is zero",
        ),
        (
            "a zero attempt",
            |run| run["run_attempt"] = json!(0),
            "a run attempt is zero",
        ),
    ];
    for (case, edit, detail) in cases {
        let run = read(edit);

        assert!(
            matches!(&run, Err(ForgeError::Malformed { operation: Operation::ListRuns, detail: said })
                if *said == detail),
            "{case}: {run:?}"
        );
    }
}

#[test]
fn a_run_without_the_fields_provenance_reads_is_refused_as_undecodable() {
    for field in ["path", "event", "run_attempt", "repository"] {
        let run = read(|run| {
            run.as_object_mut()
                .expect("a run is an object")
                .remove(field);
        });

        assert!(
            matches!(
                &run,
                Err(ForgeError::Json {
                    operation: Operation::ListRuns,
                    ..
                })
            ),
            "{field}: {run:?}"
        );
    }
}
