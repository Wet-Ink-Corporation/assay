//! One function for Z3 and Z4 (ADR-08): the same inputs give the same result, and Z4's gate
//! findings only remove duplicates.

use std::error::Error;

use assay_app::FindingRef;
use assay_domain::{Disposition, Finding};
use serde_json::{Value, json};

use super::fixture::{Run, document, finding, finding_with};

/// A run's documents: a kept finding, a moved one, a capped one and a dropped one.
fn documents() -> Vec<Vec<u8>> {
    let findings: [Value; 4] = [
        finding(),
        finding_with(json!({ "line": 7 })),
        finding_with(json!({ "trigger": null, "class": "logic" })),
        finding_with(json!({ "evidence": "absent();" })),
    ];
    vec![document(&findings), b"not json".to_vec()]
}

#[test]
fn z3_and_z4_validate_the_same_inputs_identically() -> Result<(), Box<dyn Error>> {
    let (z3, z4) = (Run::default(), Run::default());

    let (in_z3, in_z4) = (z3.validate(&documents())?, z4.validate(&documents())?);

    assert_eq!(in_z3, in_z4);
    Ok(())
}

#[test]
fn z4_differs_from_z3_only_by_duplicates_of_gate_findings() -> Result<(), Box<dyn Error>> {
    let gate: Finding = serde_json::from_value(finding_with(json!({ "line": 3 })))?;
    let z4 = Run {
        gate_findings: vec![gate],
        ..Run::default()
    };

    let (in_z3, in_z4) = (
        Run::default().validate(&documents())?,
        z4.validate(&documents())?,
    );

    let duplicates: Vec<FindingRef> = in_z4
        .transitions
        .iter()
        .filter(|t| t.code == Disposition::DuplicateOfGate)
        .map(|t| t.at)
        .collect();
    let mut expected = in_z3;
    expected
        .findings
        .retain(|kept| !duplicates.contains(&kept.at));
    let without_duplicates: Vec<_> = in_z4
        .transitions
        .iter()
        .filter(|t| t.code != Disposition::DuplicateOfGate)
        .copied()
        .collect();
    assert_eq!(
        (duplicates, in_z4.findings, without_duplicates),
        (
            vec![FindingRef::Finding {
                document: 0,
                index: 0
            }],
            expected.findings,
            expected.transitions
        )
    );
    Ok(())
}
