//! Which fingerprints an owner's `/assay waive` may clear (R-08(a), #59, 04 §5 waive), as
//! `assay status` reports them: exactly the fingerprints `decide`'s waive would clear.

use std::collections::BTreeSet;
use std::error::Error;

use assay_domain::{
    Fingerprint, GateFinding, GateReport, Severity, Waiver, WaiverRefusal, decide,
    waivable_fingerprints, waiver_refusal,
};

use crate::support::{attested_gate, clean_inputs, fp, gate_finding, rubric_v1};

/// The five human-decision rules of R-08(a), as the temper adapter writes them (03 §2.1).
const HUMAN_DECISION_RULES: [&str; 5] = [
    "raised",
    "exception",
    "gate-config",
    "test-inventory",
    "manifest-switch",
];

/// Rules R-08(a) never waives: clippy, build, test and deny failures, and the other temper rules.
const NEVER_WAIVABLE_RULES: [&str; 6] = [
    "clippy::unwrap_used",
    "build",
    "tests",
    "deny",
    "lints-table",
    "lint-inheritance",
];

fn fp_of(index: usize) -> Fingerprint {
    fp(&format!("{index:016x}"))
}

fn temper(findings: Vec<GateFinding>) -> GateReport {
    attested_gate("temper", findings)
}

#[test]
fn each_human_decision_rule_of_temper_is_waivable() -> Result<(), Box<dyn Error>> {
    let findings = HUMAN_DECISION_RULES
        .iter()
        .enumerate()
        .map(|(index, rule)| gate_finding(&format!("{index:016x}"), rule, Severity::Blocker))
        .collect();

    let waivable = waivable_fingerprints(&rubric_v1()?, &[temper(findings)]);

    let expected: BTreeSet<Fingerprint> = (0..HUMAN_DECISION_RULES.len()).map(fp_of).collect();
    assert_eq!(waivable, expected);
    Ok(())
}

#[test]
fn clippy_build_test_and_deny_findings_are_never_waivable() -> Result<(), Box<dyn Error>> {
    let findings = NEVER_WAIVABLE_RULES
        .iter()
        .enumerate()
        .map(|(index, rule)| gate_finding(&format!("{index:016x}"), rule, Severity::Blocker))
        .collect();

    let waivable = waivable_fingerprints(&rubric_v1()?, &[temper(findings)]);

    assert_eq!(waivable, BTreeSet::new());
    Ok(())
}

#[test]
fn a_human_decision_rule_of_another_gate_is_not_waivable() -> Result<(), Box<dyn Error>> {
    let custom = attested_gate(
        "custom-lint",
        vec![gate_finding("00000000000000a1", "raised", Severity::Major)],
    );

    let waivable = waivable_fingerprints(&rubric_v1()?, &[custom]);

    assert_eq!(waivable, BTreeSet::new());
    Ok(())
}

#[test]
fn a_gate_finding_without_a_rule_is_not_waivable() -> Result<(), Box<dyn Error>> {
    let finding = GateFinding {
        fp: fp("00000000000000a2"),
        rule: None,
        severity: Severity::Major,
    };

    let waivable = waivable_fingerprints(&rubric_v1()?, &[temper(vec![finding])]);

    assert_eq!(waivable, BTreeSet::new());
    Ok(())
}

#[test]
fn a_fingerprint_shared_with_an_unwaivable_finding_is_not_waivable() -> Result<(), Box<dyn Error>> {
    let shared = "00000000000000c1";
    let gates = [
        temper(vec![gate_finding(shared, "exception", Severity::Major)]),
        attested_gate(
            "custom-lint",
            vec![gate_finding(shared, "raised", Severity::Major)],
        ),
    ];

    let waivable = waivable_fingerprints(&rubric_v1()?, &gates);

    assert_eq!(waivable, BTreeSet::new());
    Ok(())
}

#[test]
fn every_fingerprint_is_waivable_exactly_when_decide_would_clear_its_waiver()
-> Result<(), Box<dyn Error>> {
    let rubric = rubric_v1()?;
    let mut inputs = clean_inputs();
    let mut findings: Vec<GateFinding> = HUMAN_DECISION_RULES
        .iter()
        .chain(NEVER_WAIVABLE_RULES.iter())
        .enumerate()
        .map(|(index, rule)| gate_finding(&format!("{index:016x}"), rule, Severity::Major))
        .collect();
    findings.push(gate_finding("0000000000000000", "build", Severity::Major));
    inputs.gates = vec![temper(findings)];
    let total = HUMAN_DECISION_RULES.len() + NEVER_WAIVABLE_RULES.len();

    let waivable = waivable_fingerprints(&rubric, &inputs.gates);

    for index in 0..total {
        let mut waived = inputs.clone();
        waived.waivers = vec![Waiver {
            fp: fp_of(index),
            by: "owner".to_owned(),
        }];
        let cleared = decide(&rubric, &waived)
            .waived
            .iter()
            .any(|entry| entry.fp == fp_of(index));
        assert_eq!(
            waivable.contains(&fp_of(index)),
            cleared,
            "fingerprint {index}: status and decide disagree"
        );
        assert_eq!(
            waiver_refusal(&rubric, &inputs.gates, fp_of(index)).is_none(),
            cleared,
            "fingerprint {index}: the refusal and decide disagree"
        );
    }
    Ok(())
}

// --- why a waiver cannot apply (#59: the verdict summary shows it) ---

#[test]
fn a_waiver_of_a_clippy_finding_is_refused_naming_its_gate_and_rule() -> Result<(), Box<dyn Error>>
{
    let gates = [temper(vec![gate_finding(
        "00000000000000a1",
        "clippy::unwrap_used",
        Severity::Blocker,
    )])];

    let refusal = waiver_refusal(&rubric_v1()?, &gates, fp("00000000000000a1"));

    assert_eq!(
        refusal,
        Some(WaiverRefusal::NotWaivable {
            gate: "temper",
            rule: Some("clippy::unwrap_used"),
        })
    );
    Ok(())
}

#[test]
fn a_waiver_shared_with_a_clippy_finding_is_refused_for_that_finding() -> Result<(), Box<dyn Error>>
{
    let gates = [temper(vec![
        gate_finding("00000000000000a1", "exception", Severity::Blocker),
        gate_finding("00000000000000a1", "clippy::unwrap_used", Severity::Blocker),
    ])];

    let refusal = waiver_refusal(&rubric_v1()?, &gates, fp("00000000000000a1"));

    assert_eq!(
        refusal,
        Some(WaiverRefusal::NotWaivable {
            gate: "temper",
            rule: Some("clippy::unwrap_used"),
        })
    );
    Ok(())
}

#[test]
fn a_waiver_of_a_finding_with_no_rule_is_refused() -> Result<(), Box<dyn Error>> {
    let finding = GateFinding {
        fp: fp("00000000000000a1"),
        rule: None,
        severity: Severity::Blocker,
    };

    let gates = [temper(vec![finding])];

    let refusal = waiver_refusal(&rubric_v1()?, &gates, fp("00000000000000a1"));

    assert_eq!(
        refusal,
        Some(WaiverRefusal::NotWaivable {
            gate: "temper",
            rule: None,
        })
    );
    Ok(())
}

#[test]
fn a_waiver_naming_no_gate_finding_is_refused() -> Result<(), Box<dyn Error>> {
    let gates = [temper(vec![gate_finding(
        "00000000000000a1",
        "raised",
        Severity::Blocker,
    )])];

    let refusal = waiver_refusal(&rubric_v1()?, &gates, fp("00000000000000b2"));

    assert_eq!(refusal, Some(WaiverRefusal::NoGateFinding));
    Ok(())
}

#[test]
fn a_waiver_of_a_human_decision_finding_is_not_refused() -> Result<(), Box<dyn Error>> {
    let gates = [temper(vec![gate_finding(
        "00000000000000a1",
        "raised",
        Severity::Blocker,
    )])];

    let refusal = waiver_refusal(&rubric_v1()?, &gates, fp("00000000000000a1"));

    assert_eq!(refusal, None);
    Ok(())
}
