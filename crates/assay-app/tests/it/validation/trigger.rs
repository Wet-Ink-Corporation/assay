//! Step 4: from `major` up a trigger is required, else the finding is capped at `minor`.

use std::error::Error;

use assay_app::{FindingRef, Transition};
use assay_domain::{Disposition, Severity};
use serde_json::json;

use super::fixture::{finding_with, validate_one};

#[test]
fn caps_a_major_without_a_trigger_at_minor() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(json!({ "trigger": null }))])?;

    let severities: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.severity)
        .collect();
    assert_eq!(
        (severities, validated.transitions),
        (
            vec![Severity::Minor],
            vec![Transition {
                at: FindingRef::Finding {
                    document: 0,
                    index: 0
                },
                code: Disposition::NoTrigger,
                from: Some(Severity::Major),
                to: Some(Severity::Minor),
            }]
        )
    );
    Ok(())
}

#[test]
fn caps_a_blocker_whose_trigger_is_blank() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(
        json!({ "severity": "blocker", "trigger": "  \n" }),
    )])?;

    let moves: Vec<_> = validated
        .transitions
        .iter()
        .map(|t| (t.code, t.from, t.to))
        .collect();
    assert_eq!(
        moves,
        [(
            Disposition::NoTrigger,
            Some(Severity::Blocker),
            Some(Severity::Minor)
        )]
    );
    Ok(())
}

#[test]
fn leaves_a_minor_without_a_trigger_alone() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(
        json!({ "severity": "minor", "trigger": null }),
    )])?;

    let severities: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.severity)
        .collect();
    assert_eq!(
        (severities, validated.transitions),
        (vec![Severity::Minor], vec![])
    );
    Ok(())
}
