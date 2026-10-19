//! The App's check: found by (head, name) and app id, created when absent, and written by
//! compare-and-set on its `external_id` (02 §5, T7).

use assay_app::{
    CheckConclusion, CheckFound, CheckName, CheckRun, CheckRunId, CheckState, CheckStatus,
    CheckUpdate, CompareAndSetError, Forge, VerdictForge,
};
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::json;

use super::recorded::{
    HEAD, Recorded, Waits, first_write, fixture, fixture_json, github, head, id, json, path, reply,
};

/// The verdict check's name on the default branch.
fn verdict() -> CheckName {
    CheckName::for_base("main", "main")
}

/// The listing of check runs named `assay / verdict` on [`HEAD`].
fn listing() -> String {
    path(&format!(
        "commits/{HEAD}/check-runs?filter=all&per_page=100&check_name=assay%20%2F%20verdict"
    ))
}

/// The App's recorded check.
fn app_check() -> CheckRun {
    CheckRun {
        id: CheckRunId(id(117_798_470_001)),
        external_id: "pending:39168909542:3".to_owned(),
        status: CheckStatus::InProgress,
    }
}

/// A decision `decide` writes.
fn decision() -> CheckUpdate {
    CheckUpdate {
        state: CheckState::Completed(CheckConclusion::Failure),
        title: "assay: failure (gate-error)".to_owned(),
        summary: "A gate failed.".to_owned(),
        text: "evidence: none".to_owned(),
        external_id: "0f".repeat(32),
    }
}

/// What `open` stamps.
fn pending() -> CheckUpdate {
    CheckUpdate {
        state: CheckState::InProgress,
        title: "assay: pending".to_owned(),
        summary: "Review in progress.".to_owned(),
        text: String::new(),
        external_id: "pending:39168909542:3".to_owned(),
    }
}

#[test]
fn a_same_named_check_from_another_app_is_ignored() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &listing(),
        fixture("check_runs_same_name.json"),
    );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_check(&head(), &verdict());

    assert_eq!(found.expect("listed"), Some(app_check()));
}

#[test]
fn only_another_apps_check_is_no_check() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &listing(),
        fixture("check_runs_foreign_only.json"),
    );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_check(&head(), &verdict());

    assert_eq!(found.expect("listed"), None);
}

#[test]
fn two_app_checks_of_one_name_are_refused() {
    let recorded =
        Recorded::default().ok(Method::Get, &listing(), fixture("check_runs_two_app.json"));
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_check(&head(), &verdict());

    assert!(
        matches!(
            found,
            Err(ForgeError::Ambiguous {
                operation: Operation::ListChecks
            })
        ),
        "{found:?}"
    );
}

#[test]
fn the_listing_follows_the_next_page() {
    let next = format!(
        "repositories/1413773798/commits/{HEAD}/check-runs?filter=all&per_page=100&check_name=assay%20%2F%20verdict&page=2"
    );
    let link = format!("<https://api.github.com/{next}>; rel=\"next\"");
    let page = json!({
        "total_count": 1,
        "check_runs": [fixture_json("check_run_app.json")],
    });
    let recorded = Recorded::default()
        .then(
            Method::Get,
            &listing(),
            reply(
                200,
                &[("Link", &link)],
                fixture("check_runs_foreign_only.json"),
            ),
        )
        .ok(Method::Get, &next, json(&page));
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_check(&head(), &verdict());

    assert_eq!(found.expect("listed"), Some(app_check()));
    assert_eq!(recorded.unused(), 0, "both pages read");
}

#[test]
fn a_next_page_off_the_api_is_refused() {
    let recorded = Recorded::default().then(
        Method::Get,
        &listing(),
        reply(
            200,
            &[("link", "<https://example.com/steal?page=2>; rel=\"next\"")],
            fixture("check_runs_foreign_only.json"),
        ),
    );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_check(&head(), &verdict());

    assert!(
        matches!(found, Err(ForgeError::Malformed { detail, .. }) if detail == "next page is not on api.github.com"),
        "{found:?}"
    );
}

#[test]
fn an_update_overwrites_status_conclusion_output_and_external_id_without_annotations() {
    let recorded = Recorded::default().ok(
        Method::Patch,
        &path("check-runs/117798470001"),
        fixture("check_run_app.json"),
    );
    let waits = Waits::default();

    github(&recorded, &waits)
        .update_check(app_check().id, &decision())
        .expect("updated");

    assert_eq!(
        first_write(&recorded),
        Some(json!({
            "status": "completed",
            "conclusion": "failure",
            "external_id": "0f".repeat(32),
            "output": {
                "title": "assay: failure (gate-error)",
                "summary": "A gate failed.",
                "text": "evidence: none",
            },
        }))
    );
}

#[test]
fn a_compare_and_set_conflict_writes_nothing() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("check-runs/117798470001"),
        fixture("check_run_app.json"),
    );
    let waits = Waits::default();

    let written = github(&recorded, &waits).update_check_if(
        app_check().id,
        "pending:39168909542:2",
        &decision(),
    );

    assert!(
        matches!(&written, Err(CompareAndSetError::Conflict { found }) if found == "pending:39168909542:3"),
        "{written:?}"
    );
    assert_eq!(recorded.writes(), Vec::new(), "no write after a conflict");
}

#[test]
fn a_compare_and_set_on_another_apps_check_writes_nothing() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("check-runs/117798464273"),
        fixture("check_run_foreign.json"),
    );
    let waits = Waits::default();
    let foreign = CheckRunId(id(117_798_464_273));

    let written = github(&recorded, &waits).update_check_if(
        foreign,
        "d2defc3b-9472-58da-bec7-4caf3d4b8da8",
        &decision(),
    );

    assert!(
        matches!(&written, Err(CompareAndSetError::Forge(ForgeError::ForeignCheck { id: refused })) if *refused == foreign),
        "{written:?}"
    );
    assert_eq!(
        recorded.writes(),
        Vec::new(),
        "another app's check is never written"
    );
}

#[test]
fn a_compare_and_set_with_the_read_stamp_writes() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("check-runs/117798470001"),
            fixture("check_run_app.json"),
        )
        .ok(
            Method::Patch,
            &path("check-runs/117798470001"),
            fixture("check_run_app.json"),
        );
    let waits = Waits::default();

    let written = github(&recorded, &waits).update_check_if(
        app_check().id,
        "pending:39168909542:3",
        &decision(),
    );

    assert!(written.is_ok(), "{written:?}");
    assert_eq!(recorded.writes().len(), 1, "one PATCH");
}

#[test]
fn find_or_create_returns_the_apps_check_without_writing() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &listing(),
        fixture("check_runs_same_name.json"),
    );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_or_create_check(&head(), &verdict(), &pending());

    assert_eq!(found.expect("found"), CheckFound::Found(app_check()));
    assert_eq!(recorded.writes(), Vec::new(), "nothing created");
}

#[test]
fn find_or_create_creates_the_apps_check_beside_another_apps() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &listing(),
            fixture("check_runs_foreign_only.json"),
        )
        .then(
            Method::Post,
            &path("check-runs"),
            reply(201, &[], fixture("check_run_app.json")),
        );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_or_create_check(&head(), &verdict(), &pending());

    assert_eq!(found.expect("created"), CheckFound::Created(app_check()));
    assert_eq!(
        first_write(&recorded),
        Some(json!({
            "name": "assay / verdict",
            "head_sha": HEAD,
            "status": "in_progress",
            "external_id": "pending:39168909542:3",
            "output": {"title": "assay: pending", "summary": "Review in progress.", "text": ""},
        }))
    );
}

#[test]
fn a_created_check_another_app_answers_for_is_refused() {
    let mut renamed = fixture_json("check_run_foreign.json");
    renamed["name"] = json!("assay / verdict");
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &listing(),
            fixture("check_runs_foreign_only.json"),
        )
        .then(
            Method::Post,
            &path("check-runs"),
            reply(201, &[], json(&renamed)),
        );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_or_create_check(&head(), &verdict(), &pending());

    assert!(
        matches!(found, Err(ForgeError::ForeignCheck { .. })),
        "{found:?}"
    );
}

#[test]
fn a_create_that_landed_before_a_5xx_is_not_posted_again() {
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
        )
        .ok(
            Method::Get,
            &listing(),
            fixture("check_runs_same_name.json"),
        );
    let waits = Waits::default();

    let found = github(&recorded, &waits).find_or_create_check(&head(), &verdict(), &pending());

    assert_eq!(found.expect("created"), CheckFound::Created(app_check()));
    assert_eq!(recorded.writes().len(), 1, "one POST only");
}
