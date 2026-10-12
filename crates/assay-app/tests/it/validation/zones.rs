//! One function for Z3 and Z4 (ADR-08): Z4's inputs are Z3's plus the gate findings, and those
//! only remove duplicates.

use std::error::Error;

use assay_app::{FindingRef, Transition, ValidFinding, Validated};
use assay_domain::{Disposition, Finding};
use serde_json::{Value, json};

use super::fixture::{Run, document, finding, finding_with};

/// A run's reviewer documents: a kept finding, a moved one, a capped one, a dropped one and a
/// document that fails its schema.
fn documents() -> Vec<Vec<u8>> {
    let findings: [Value; 4] = [
        finding(),
        finding_with(json!({ "line": 7 })),
        finding_with(json!({ "trigger": null, "class": "logic" })),
        finding_with(json!({ "evidence": "absent_function();" })),
    ];
    vec![document(&findings), b"not json".to_vec()]
}

/// What Z3 validates with: no gate findings exist yet when it runs (04 §1).
fn z3() -> Run<'static> {
    Run::default()
}

/// What Z4 validates with: Z3's inputs plus `gate_findings`.
fn z4(gate_findings: Vec<Finding>) -> Run<'static> {
    Run {
        gate_findings,
        ..z3()
    }
}

/// The findings of `validated` that are not at `removed`, and its transitions that do not
/// remove one.
fn without(validated: Validated, removed: &[FindingRef]) -> (Vec<ValidFinding>, Vec<Transition>) {
    let kept = validated
        .findings
        .into_iter()
        .filter(|kept| !removed.contains(&kept.at))
        .collect();
    let transitions = validated
        .transitions
        .into_iter()
        .filter(|t| t.code != Disposition::DuplicateOfGate)
        .collect();
    (kept, transitions)
}

#[test]
fn z4_without_gate_findings_validates_exactly_as_z3() -> Result<(), Box<dyn Error>> {
    let (in_z3, in_z4) = (
        z3().validate(&documents())?,
        z4(Vec::new()).validate(&documents())?,
    );

    assert_eq!(in_z4, in_z3);
    Ok(())
}

#[test]
fn z4_differs_from_z3_only_by_duplicates_of_gate_findings() -> Result<(), Box<dyn Error>> {
    // Duplicates the first finding (line 2, same class) and nothing else.
    let gate: Finding = serde_json::from_value(finding_with(json!({ "line": 3 })))?;

    let (in_z3, in_z4) = (
        z3().validate(&documents())?,
        z4(vec![gate]).validate(&documents())?,
    );

    let removed: Vec<FindingRef> = in_z4
        .transitions
        .iter()
        .filter(|t| t.code == Disposition::DuplicateOfGate)
        .map(|t| t.at)
        .collect();
    let first = FindingRef::Finding {
        document: 0,
        index: 0,
    };
    assert_eq!(removed, [first]);
    assert_eq!(without(in_z4, &removed), without(in_z3, &removed));
    Ok(())
}
