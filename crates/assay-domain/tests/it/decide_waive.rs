//! Waivers are bound to one gate and one waivable rule: a fingerprint names a finding, not a
//! gate or a rule, so a colliding fingerprint never clears anything else
//! (`docs/architecture/04-pipeline.md` §5 waive).

use std::error::Error;

use assay_domain::{
    Conclusion, Counts, Decision, DecisionInputs, GateConclusion, GateOutcome, ReasonCode,
    Severity, Waived, Waiver, decide,
};

use crate::support::{clean_inputs, gate, gate_finding, reason, rubric_v1};

const SHARED_FP: &str = "00000000000000c1";

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

fn waiver(fp: &str, by: &str) -> Waiver {
    Waiver {
        fp: fp.to_owned(),
        by: by.to_owned(),
    }
}

#[test]
fn a_waiver_does_not_clear_a_colliding_finding_of_another_gate() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![
        gate(
            "temper",
            GateConclusion::Fail,
            vec![gate_finding(SHARED_FP, "exception", Severity::Major)],
        ),
        gate(
            "custom-lint",
            GateConclusion::Fail,
            vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
        ),
    ];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::GateFailed, "custom-lint")]
    );
    Ok(())
}

#[test]
fn a_waiver_does_not_clear_a_colliding_non_waivable_finding_of_the_same_gate()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![gate(
        "temper",
        GateConclusion::Fail,
        vec![
            gate_finding(SHARED_FP, "exception", Severity::Major),
            gate_finding(SHARED_FP, "clippy::unwrap_used", Severity::Blocker),
        ],
    )];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    assert_eq!(
        decision.counts,
        Counts {
            blocker: 1,
            major: 0,
            minor: 0,
            nit: 0,
        },
        "the clippy finding still counts; the waived exception does not"
    );
    Ok(())
}

#[test]
fn a_gate_with_a_failed_step_stays_failed_when_its_findings_are_waived()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    let mut temper = gate(
        "temper",
        GateConclusion::Fail,
        vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
    );
    temper.outcome = GateOutcome::Ran {
        conclusion: GateConclusion::Fail,
        failed_steps: vec!["fmt".to_owned()],
    };
    inputs.gates = vec![temper];
    inputs.waivers = vec![waiver(SHARED_FP, "owner")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    Ok(())
}

#[test]
fn a_waiver_without_an_actor_is_not_applied() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.gates = vec![gate(
        "temper",
        GateConclusion::Fail,
        vec![gate_finding(SHARED_FP, "raised", Severity::Major)],
    )];
    inputs.waivers = vec![waiver(SHARED_FP, "")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.waived, Vec::<Waived>::new());
    Ok(())
}
