//! What `decide` believes of a gate document's `failed_steps` (#153, 03 §2.1): the temper
//! adapter's attestation becomes `StepAttribution::Known`, so a waiver can clear a failed temper
//! gate whose every failure cause is a waived finding; any other gate, or a document whose own
//! conclusion is not the gate's, stays `Unknown`.

use std::error::Error;

use assay_app::ArtifactJob;
use assay_domain::{
    Conclusion as Verdict, GateConclusion, GateOutcome, RequiredRole, StepAttribution, Waiver,
    decide,
};
use serde_json::{Value, json};

use crate::decide::inputs;
use crate::decide_fakes::{Scenario, authenticated, gate_finding};
use crate::plan_fakes::{FakePr, HEAD};
use crate::support::rubric_v1;

/// A gate document from `gate` concluding `conclusion` with `findings` and `failed_steps`.
fn attested_document(
    gate: &str,
    conclusion: &str,
    findings: &Value,
    failed_steps: &[&str],
) -> Vec<u8> {
    json!({
        "schema": "assay.findings/1",
        "source": {"kind": "gate", "name": gate, "version": "git:x", "run_key": "run-42-1"},
        "conclusion": conclusion,
        "findings": findings,
        "failed_steps": failed_steps,
    })
    .to_string()
    .into_bytes()
}

/// The outcome `decide` reads for the gate at `index` after `job` published `document` for
/// `plugin` with `reported` as the job's conclusion.
fn outcome_of(
    (job, plugin, index): (ArtifactJob, &str, usize),
    reported: GateConclusion,
    document: Vec<u8>,
) -> Result<GateOutcome, Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.publish_gate((job, plugin), reported, document)?;

    let run = authenticated(scenario.authenticate()?)?;

    let gate = run.gates().get(index).ok_or("the gate")?;
    Ok(gate.outcome.clone())
}

const LINT_TEMPER: (ArtifactJob, &str, usize) = (ArtifactJob::GatesLint, "temper", 1);
const FAIL: GateConclusion = GateConclusion::Fail;

fn ran(conclusion: GateConclusion, steps: StepAttribution) -> GateOutcome {
    GateOutcome::Ran { conclusion, steps }
}

#[test]
fn a_temper_gate_attesting_no_failed_step_is_known_and_empty() -> Result<(), Box<dyn Error>> {
    let document = attested_document("temper", "fail", &json!([]), &[]);

    let outcome = outcome_of(LINT_TEMPER, FAIL, document)?;

    assert_eq!(outcome, ran(FAIL, StepAttribution::Known(Vec::new())));
    Ok(())
}

#[test]
fn a_temper_gate_naming_failed_steps_is_known_with_them() -> Result<(), Box<dyn Error>> {
    let document = attested_document("temper", "fail", &json!([]), &["fmt", "baseline"]);

    let outcome = outcome_of(LINT_TEMPER, FAIL, document)?;

    let steps = StepAttribution::Known(vec!["fmt".to_owned(), "baseline".to_owned()]);
    assert_eq!(outcome, ran(FAIL, steps));
    Ok(())
}

#[test]
fn a_temper_document_without_failed_steps_is_unknown() -> Result<(), Box<dyn Error>> {
    let document = json!({
        "schema": "assay.findings/1",
        "source": {"kind": "gate", "name": "temper", "version": "git:x", "run_key": "run-42-1"},
        "conclusion": "fail",
        "findings": [],
    });

    let outcome = outcome_of(LINT_TEMPER, FAIL, document.to_string().into_bytes())?;

    assert_eq!(outcome, ran(FAIL, StepAttribution::Unknown));
    Ok(())
}

#[test]
fn an_exec_gate_attesting_its_steps_is_not_believed() -> Result<(), Box<dyn Error>> {
    let document = attested_document("custom-lint", "fail", &json!([]), &[]);

    let outcome = outcome_of((ArtifactJob::GatesTest, "custom-lint", 2), FAIL, document)?;

    assert_eq!(
        outcome,
        ran(FAIL, StepAttribution::Unknown),
        "only the temper adapter attests step results"
    );
    Ok(())
}

#[test]
fn a_job_failing_over_a_passing_attested_document_is_unknown() -> Result<(), Box<dyn Error>> {
    let document = attested_document("temper", "pass", &json!([]), &[]);

    let outcome = outcome_of(LINT_TEMPER, FAIL, document)?;

    assert_eq!(
        outcome,
        ran(FAIL, StepAttribution::Unknown),
        "the document attests a pass, so nothing attests what failed"
    );
    Ok(())
}

#[test]
fn a_passing_document_naming_a_failed_step_is_an_invalid_document() -> Result<(), Box<dyn Error>> {
    let document = attested_document("temper", "pass", &json!([]), &["fmt"]);

    let outcome = outcome_of(LINT_TEMPER, GateConclusion::Pass, document)?;

    assert_eq!(
        outcome,
        GateOutcome::Error {
            detail: "invalid-document".to_owned()
        }
    );
    Ok(())
}

/// A temper gate in gates-static that failed on one `raised` finding, attesting `failed_steps`,
/// decided with an owner's waiver of that finding.
fn waived_static_failure(failed_steps: &[&str]) -> Result<Verdict, Box<dyn Error>> {
    let mut pr = FakePr::new();
    pr.write(HEAD, "src/lib.rs", b"pub fn one() {}\n");
    let mut scenario = Scenario::of(pr)?;
    let findings = json!([gate_finding("raised", "src/lib.rs", 1, "pub fn one() {}")]);
    scenario.publish_gate(
        (ArtifactJob::GatesStatic, "temper"),
        FAIL,
        attested_document("temper", "fail", &findings, failed_steps),
    )?;
    let run = authenticated(scenario.authenticate()?)?;
    let gate = run.gates().first().ok_or("the static gate")?;
    let fp = gate.findings.first().ok_or("the raised finding")?.fp;
    let mut inputs = inputs(&run, &[RequiredRole::CombinedMicro]);
    inputs.waivers = vec![Waiver {
        fp,
        by: "owner".to_owned(),
    }];

    Ok(decide(&rubric_v1()?, &inputs).conclusion)
}

#[test]
fn a_waiver_clears_a_temper_gate_that_failed_only_on_the_waived_finding()
-> Result<(), Box<dyn Error>> {
    let verdict = waived_static_failure(&[])?;

    assert_eq!(verdict, Verdict::Success);
    Ok(())
}

#[test]
fn a_waiver_never_clears_a_temper_gate_with_a_failed_step() -> Result<(), Box<dyn Error>> {
    let verdict = waived_static_failure(&["fmt"])?;

    assert_eq!(verdict, Verdict::Failure);
    Ok(())
}
