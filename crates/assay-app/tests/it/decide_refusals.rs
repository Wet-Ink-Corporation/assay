//! One test per refusal branch of `authenticate` the main suite does not reach: each asserts the
//! exact `Refusal`, `GateOutcome` or error it ends in (review round 1 of #56).

use std::assert_matches;
use std::error::Error;

use assay_app::{
    ArtifactJob, Authentication, DecideError, GateError, JobResult, MAX_DOCUMENT_BYTES,
    MAX_FETCHED_BYTES, MAX_REVIEW_DOCUMENTS, Moved, OutputsProblem, Refusal, Reported,
    ReportedGate, Sha256Digest, StateStep, Unreadable, UpstreamJob,
};
use assay_domain::{GateConclusion, GateOutcome};
use serde_json::json;

use crate::decide_fakes::{Scenario, authenticated, gate_document, refusal};

/// Bytes one past what a single document may hold.
fn oversized() -> Vec<u8> {
    vec![b' '; MAX_DOCUMENT_BYTES + 1]
}

// --- GitHub's live state ---

#[test]
fn a_moved_head_and_base_supersede_the_run_as_both() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.state.current.head_sha = "cccccccccccccccccccccccccccccccccccccccc".parse()?;
    "release".clone_into(&mut scenario.state.current.base_ref);

    let outcome = scenario.authenticate()?;

    assert_matches!(
        outcome,
        Ok(Authentication::Superseded(Moved::HeadAndBaseRef))
    );
    Ok(())
}

#[test]
fn an_unreadable_pull_request_is_an_error_naming_the_call() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.state.failing = true;

    let outcome = scenario.authenticate()?;

    assert_matches!(
        outcome,
        Err(DecideError::State {
            step: StateStep::Current,
            ..
        })
    );
    Ok(())
}

// --- job outputs ---

#[test]
fn a_skipped_gates_job_that_reports_a_gate_is_refused() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_lint.result = JobResult::Skipped;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::GatesLint,
            problem: OutputsProblem::SkippedJobReported,
        }
    );
    Ok(())
}

#[test]
fn a_gate_reported_twice_by_its_job_is_refused() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_static.gates.push(ReportedGate {
        plugin: "temper".to_owned(),
        reported: Reported::Error(GateError::NotRun),
    });

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::GatesStatic,
            problem: OutputsProblem::DuplicatePlugin,
        }
    );
    Ok(())
}

#[test]
fn a_successful_gates_job_that_omits_a_scheduled_gate_is_refused() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.gates_test.gates.clear();

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::GatesTest,
            problem: OutputsProblem::MissingPlugin,
        }
    );
    Ok(())
}

#[test]
fn more_review_documents_than_the_cap_are_refused_unread() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.review.documents = (0..=MAX_REVIEW_DOCUMENTS)
        .map(|index| assay_app::ArtifactDocument {
            file: format!("r{index}.json"),
            digest: Sha256Digest::of(b""),
        })
        .collect();

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::Review,
            problem: OutputsProblem::TooManyDocuments,
        }
    );
    assert_eq!(
        scenario.artifacts.fetches.borrow().len(),
        4,
        "the plan and three gate documents only: no review document is fetched"
    );
    Ok(())
}

#[test]
fn a_review_document_named_twice_is_refused() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_review("combined.json", b"{}".to_vec())?;
    scenario.publish_review("combined.json", b"{}".to_vec())?;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::Review,
            problem: OutputsProblem::DuplicateFile,
        }
    );
    Ok(())
}

#[test]
fn a_skipped_review_job_that_reports_documents_is_refused() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_review("combined.json", b"{}".to_vec())?;
    scenario.request.review.result = JobResult::Skipped;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::Outputs {
            job: UpstreamJob::Review,
            problem: OutputsProblem::SkippedJobReported,
        }
    );
    Ok(())
}

// --- documents too large to read ---

#[test]
fn an_oversized_plan_is_refused_as_too_large() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_plan(oversized());

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::ArtifactUnreadable {
            job: ArtifactJob::Plan,
            ref file,
            why: Unreadable::TooLarge,
        } if file == "run.json"
    );
    Ok(())
}

#[test]
fn an_oversized_review_document_is_refused_as_too_large() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_review("combined.json", oversized())?;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::ArtifactUnreadable {
            job: ArtifactJob::Review,
            ref file,
            why: Unreadable::TooLarge,
        } if file == "combined.json"
    );
    Ok(())
}

#[test]
fn an_oversized_gate_document_is_a_gate_error() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate(
        (ArtifactJob::GatesLint, "temper"),
        GateConclusion::Pass,
        oversized(),
    )?;

    let run = authenticated(scenario.authenticate()?)?;

    let lint = run.gates().get(1).ok_or("the lint gate")?;
    assert_eq!(
        lint.outcome,
        GateOutcome::Error {
            detail: "document-too-large".to_owned()
        }
    );
    Ok(())
}

#[test]
fn an_adapter_returning_more_than_the_limit_is_held_to_it() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    // The bytes hash to the published digest: only their size can refuse them.
    scenario.publish_plan(oversized());
    scenario.artifacts.ignore_limit = true;

    let refused = refusal(scenario.authenticate()?)?;

    assert_matches!(
        refused,
        Refusal::ArtifactUnreadable {
            job: ArtifactJob::Plan,
            why: Unreadable::TooLarge,
            ..
        }
    );
    Ok(())
}

#[test]
fn every_fetch_is_limited_by_what_the_run_has_left_to_read() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let full = vec![b' '; MAX_DOCUMENT_BYTES];
    let documents = MAX_FETCHED_BYTES / MAX_DOCUMENT_BYTES;
    for index in 0..documents {
        // Each uploaded file owns its bytes.
        scenario.publish_review(&format!("r{index}.json"), full.clone())?;
    }

    let refused = refusal(scenario.authenticate()?)?;

    let fetches = scenario.artifacts.fetches.borrow();
    let mut read = 0_usize;
    for (index, &(limit, returned)) in fetches.iter().enumerate() {
        let left = MAX_FETCHED_BYTES - read;
        assert_eq!(limit, MAX_DOCUMENT_BYTES.min(left), "fetch {index}");
        read += returned.unwrap_or(0);
    }
    // The plan and gate documents were read first, so the last review document no longer fits.
    assert_matches!(
        refused,
        Refusal::ArtifactUnreadable {
            job: ArtifactJob::Review,
            ref file,
            why: Unreadable::TooLarge,
        } if *file == format!("r{}.json", documents - 1)
    );
    Ok(())
}

// --- the job's conclusion against the document's ---

#[test]
fn a_passing_job_with_a_neutral_document_is_an_invalid_document() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        GateConclusion::Pass,
        gate_document("temper", "neutral", &json!([])),
    )?;

    let run = authenticated(scenario.authenticate()?)?;

    let static_temper = run.gates().first().ok_or("the static gate")?;
    assert_eq!(
        static_temper.outcome,
        GateOutcome::Error {
            detail: "invalid-document".to_owned()
        }
    );
    Ok(())
}
