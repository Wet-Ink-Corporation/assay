//! `assay status` of a refused run (#218, 03 §6b): `refused: {reason}`, read only from the App's
//! own verdict check, so a refusal differs from a run that never decided. The reason code comes
//! from the check's title, the one place `publish` writes it as closed vocabulary alone, and only
//! when the check's text says the run persisted nothing and decided nothing; anything else is
//! refused (wi-b5affa).

use std::assert_matches;
use std::error::Error;

use assay_app::{
    JobResult, ObservedConclusion, Publication, StatusError, StatusReport, StatusUnreadable,
    Verdict, publish,
};
use serde_json::Value;

use crate::decide_fakes::{Scenario, refusal};
use crate::plan_fakes::{FakeError, HEAD};
use crate::publish_fakes::Call;
use crate::publish_support::{Run, ports, stamped, written};
use crate::status::{Ask, assert_golden, document, report, success_world};
use crate::status_fakes::{BRANCH, FETCHED_AT, FakeGitHub, REPO};

type Outcome = Result<StatusReport, StatusError<FakeError, FakeError>>;

/// What `publish` leaves for a run whose `plan` job failed: a refusal, `gate-error`.
fn refused_world() -> Result<FakeGitHub, Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.plan_job.result = JobResult::Failure;
    let refused = refusal(scenario.authenticate()?)?;
    let run = Run::new()?;
    let world = stamped()?;
    let publication = Publication {
        target: run.target(),
        verdict: Verdict::Refused(&refused),
    };
    written(publish(&publication, &ports(&world)))?;
    Ok(FakeGitHub::published(&world))
}

/// The refused world with its verdict check's title replaced by `title`.
fn with_title(title: Option<&str>) -> Result<FakeGitHub, Box<dyn Error>> {
    let mut github = refused_world()?;
    github.verdict_mut().output_title = title.map(str::to_owned);
    Ok(github)
}

fn unreadable(outcome: &Outcome) -> Option<StatusUnreadable> {
    match outcome {
        Err(StatusError::Unreadable(what)) => Some(*what),
        _ => None,
    }
}

#[test]
fn a_refused_run_matches_its_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(&refused_world()?, &Ask::new(), "refused")
}

#[test]
fn the_text_names_the_refusal() -> Result<(), Box<dyn Error>> {
    let report = report(&refused_world()?, &Ask::new())??;

    assert_eq!(
        report.text(),
        format!(
            "assay status: {REPO} #42 ({BRANCH})\n\
             Head: {HEAD} (local HEAD unknown)\n\
             Check: completed failure https://github.com/{REPO}/runs/9001\n\
             Decision: none\n\
             Refused: gate-error\n\
             Gates: none\n\
             Findings: none\n\
             Viewer: someone (other)\n\
             Fetched: {FETCHED_AT}\n"
        )
    );
    Ok(())
}

#[test]
fn a_decided_run_that_persisted_no_bundle_is_not_refused() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.fail(Call::Upload, 1);
    let persisted = written(run.publish(&world))?;
    assert!(persisted.persist.is_some(), "the upload failed");

    let status = document(&FakeGitHub::published(&world), &Ask::new())?;

    assert_eq!(status.get("refused"), None, "decided, only not persisted");
    assert_eq!(status["decision"], Value::Null);
    Ok(())
}

#[test]
fn a_decided_run_is_not_refused() -> Result<(), Box<dyn Error>> {
    let status = document(&success_world()?, &Ask::new())?;

    assert_eq!(status.get("refused"), None);
    Ok(())
}

#[test]
fn a_refusal_title_publish_does_not_write_is_refused() -> Result<(), Box<dyn Error>> {
    let titles = [
        None,
        Some(""),
        Some("assay: failure"),
        Some("assay: success (gate-error)"),
        Some("assay: failure (gate-error and 1 more)"),
        Some("assay: failure (not-a-reason)"),
        Some("assay: failure (Gate-Error)"),
        Some("assay: failure (gate-error) "),
        Some(" assay: failure (gate-error)"),
        Some("assay: failure (gate-error)\n"),
        Some("assay: failure ()"),
    ];

    for title in titles {
        let outcome = report(&with_title(title)?, &Ask::new())?;

        assert_eq!(
            unreadable(&outcome),
            Some(StatusUnreadable::CheckTitle),
            "{title:?} must be refused"
        );
    }
    Ok(())
}

#[test]
fn every_reason_code_publish_can_title_is_read_back() -> Result<(), Box<dyn Error>> {
    for code in assay_domain::ReasonCode::ALL {
        let title = format!("assay: failure ({})", code.code());

        let status = document(&with_title(Some(&title))?, &Ask::new())?;

        assert_eq!(
            status.pointer("/refused/reason"),
            Some(&Value::from(code.code())),
            "{title}"
        );
    }
    Ok(())
}

#[test]
fn a_refusal_on_a_check_that_did_not_fail_is_refused() -> Result<(), Box<dyn Error>> {
    for conclusion in [ObservedConclusion::Success, ObservedConclusion::Neutral] {
        let mut github = refused_world()?;
        github.verdict_mut().conclusion = Some(conclusion);

        let outcome = report(&github, &Ask::new())?;

        assert_eq!(
            unreadable(&outcome),
            Some(StatusUnreadable::CheckState),
            "{conclusion:?}"
        );
    }
    Ok(())
}

#[test]
fn a_text_with_a_pointer_but_no_inputs_digest_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = success_world()?;
    let check = github.verdict_mut();
    let text = check
        .output_text
        .as_deref()
        .ok_or("a decided check's text")?;
    let stripped: Vec<&str> = text
        .split('\n')
        .filter(|line| !line.starts_with("inputs_digest: "))
        .collect();
    check.output_text = Some(stripped.join("\n"));

    let outcome = report(&github, &Ask::new())?;

    assert_matches!(
        outcome,
        Err(StatusError::Unreadable(StatusUnreadable::CheckText))
    );
    Ok(())
}
