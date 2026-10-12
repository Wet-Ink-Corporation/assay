//! Step 1: a document that fails its schema is disposed of as `schema`.

use std::error::Error;

use assay_app::{FindingRef, Placement, Transition};
use assay_domain::{Disposition, EventPayload};
use serde_json::json;

use super::fixture::{Run, document, finding, finding_with, validate_one};

#[test]
fn keeps_a_valid_finding_inline_with_no_transition() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding()])?;

    assert_eq!(validated.transitions, []);
    let kept: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| (kept.at, kept.placement, kept.finding.line.get()))
        .collect();
    assert_eq!(
        kept,
        [(
            FindingRef::Finding {
                document: 0,
                index: 0
            },
            Placement::Inline,
            2
        )]
    );
    Ok(())
}

#[test]
fn disposes_of_a_document_failing_its_schema_as_one_schema_transition() -> Result<(), Box<dyn Error>>
{
    let over_length = finding_with(json!({ "title": "t".repeat(101) }));

    let validated = Run::default().validate(&[document(&[finding()]), document(&[over_length])])?;

    assert_eq!(
        validated.transitions,
        [Transition {
            at: FindingRef::Document { document: 1 },
            code: Disposition::Schema,
            from: None,
            to: None,
        }]
    );
    assert_eq!(validated.findings.len(), 1, "the valid document is kept");
    Ok(())
}

#[test]
fn disposes_of_bytes_that_are_not_json_as_schema() -> Result<(), Box<dyn Error>> {
    let validated = Run::default().validate(&[b"{not json".to_vec()])?;

    let codes: Vec<_> = validated.transitions.iter().map(|t| t.code).collect();
    assert_eq!(codes, [Disposition::Schema]);
    Ok(())
}

#[test]
fn disposes_of_a_gate_document_as_schema() -> Result<(), Box<dyn Error>> {
    let gate = json!({
        "schema": "assay.findings/1",
        "source": { "kind": "gate", "name": "temper", "version": "git:x", "run_key": "r" },
        "conclusion": "fail",
        "findings": [finding()],
    })
    .to_string()
    .into_bytes();

    let validated = Run::default().validate(&[gate])?;

    let codes: Vec<_> = validated.transitions.iter().map(|t| t.code).collect();
    assert_eq!(
        (codes, validated.findings.len()),
        (vec![Disposition::Schema], 0)
    );
    Ok(())
}

#[test]
fn emits_one_finding_disposed_event_per_transition() -> Result<(), Box<dyn Error>> {
    let validated = Run::default().validate(&[b"[]".to_vec()])?;

    let events: Vec<_> = validated.events().collect();

    assert_eq!(
        events,
        [EventPayload::FindingDisposed {
            code: Disposition::Schema,
            from: None,
            to: None
        }]
    );
    Ok(())
}
