//! Waivers apply all or nothing: a fingerprint names a finding, not a gate or a rule, so a
//! waiver clears its fingerprint only where every gate finding carrying it has a rule waivable
//! for its own gate, and otherwise clears none of them; and a failed gate is
//! cleared only when its adapter attested that no step failed without a finding
//! (`docs/architecture/04-pipeline.md` §5 waive, 03 §2.1).

use std::error::Error;

use assay_domain::{
    Conclusion, Counts, Decision, DecisionInputs, FindingDisposition, GateConclusion, GateFinding,
    GateOutcome, GateReport, ReasonCode, Severity, StepAttribution, Waived, Waiver, decide,
};

use crate::support::{attested_gate, clean_inputs, fp, gate, gate_finding, reason, rubric_v1};

const SHARED_FP: &str = "00000000000000c1";

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

fn waiver(fp_text: &str, by: &str) -> Waiver {
    Waiver {
        fp: fp(fp_text),
        by: by.to_owned(),
    }
}

fn failed_temper(steps: StepAttribution, findings: Vec<GateFinding>) -> GateReport {
    let mut temper = gate("temper", GateConclusion::Fail, findings);
    temper.outcome = GateOutcome::Ran {
        conclusion: GateConclusion::Fail,
        steps,
    };
    temper
}

#[test]
fn a_waiver_clears_nothing_when_its_fingerprint_also_names_an_unwaivable_finding_of_another_gate()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![
        attested_gate(
            "temper",
            vec![gate_finding(SHARED_FP, "exception", Severity::Major)],
        ),
        attested_gate(
            "custom-lint",
            vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
        ),
    ];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [
            reason(ReasonCode::GateFailed, "temper"),
            reason(ReasonCode::GateFailed, "custom-lint"),
        ],
        "the waiver names a fingerprint it cannot clear everywhere, so it clears it nowhere"
    );
    assert_eq!(
        decision.counts,
        Counts {
            blocker: 0,
            major: 2,
            minor: 0,
            nit: 0,
        }
    );
    Ok(())
}

#[test]
fn a_fingerprint_is_not_reported_waived_while_another_finding_carries_it_unwaived()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![
        attested_gate(
            "temper",
            vec![gate_finding(SHARED_FP, "exception", Severity::Major)],
        ),
        attested_gate(
            "custom-lint",
            vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
        ),
    ];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.waived, Vec::<Waived>::new());
    assert_eq!(decision.dispositions, Vec::<FindingDisposition>::new());
    Ok(())
}

#[test]
fn a_waiver_clears_nothing_when_its_fingerprint_also_names_an_unwaivable_finding_of_its_gate()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![attested_gate(
        "temper",
        vec![
            gate_finding(SHARED_FP, "exception", Severity::Major),
            gate_finding(SHARED_FP, "clippy::unwrap_used", Severity::Blocker),
        ],
    )];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    assert_eq!(decision.waived, Vec::<Waived>::new());
    assert_eq!(
        decision.counts,
        Counts {
            blocker: 1,
            major: 1,
            minor: 0,
            nit: 0,
        },
        "both findings still count: a waiver that cannot clear every carrier clears none"
    );
    Ok(())
}

#[test]
fn a_gate_with_a_failed_step_stays_failed_when_its_findings_are_waived()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![failed_temper(
        StepAttribution::Known(vec!["fmt".to_owned()]),
        vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
    )];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    Ok(())
}

#[test]
fn a_gate_whose_steps_are_unattested_is_never_cleared_by_waivers() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![failed_temper(
        StepAttribution::Unknown,
        vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
    )];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    Ok(())
}

#[test]
fn an_attested_gate_whose_only_finding_is_waived_is_cleared() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![failed_temper(
        StepAttribution::Known(Vec::new()),
        vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
    )];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn a_waiver_without_an_actor_is_not_applied() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![attested_gate(
        "temper",
        vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
    )];
    inputs.waivers = vec![waiver(SHARED_FP, "")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.waived, Vec::<Waived>::new());
    Ok(())
}
