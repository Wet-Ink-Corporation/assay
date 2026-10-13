//! Step 1: a document that fails its schema is disposed of as `schema`.

use std::error::Error;

use assay_app::{FindingRef, Placement, Transition};
use assay_domain::{Disposition, EventPayload};
use serde_json::json;

use super::fixture::{Run, document, document_by, finding, finding_with, validate_one};

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

// A caller is model text that only an interaction has a use for; on any other finding it would
// reach the output unchecked by step 2.
#[test]
fn rejects_a_caller_on_a_finding_that_is_not_an_interaction() -> Result<(), Box<dyn Error>> {
    let caller = json!({ "file": "src/lib.rs", "line": 2, "evidence": "cc @owner <img src=x>" });

    let (on_diff, on_absent) = (
        codes_with(json!({ "introduced_by": "diff", "caller": caller }))?,
        codes_with(json!({ "introduced_by": null, "caller": caller }))?,
    );

    assert_eq!(
        (on_diff, on_absent),
        (vec![Disposition::Schema], vec![Disposition::Schema])
    );
    Ok(())
}

#[test]
fn disposes_of_a_schema_invalid_document_as_one_transition() -> Result<(), Box<dyn Error>> {
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

/// The dispositions of one finding with `changes` applied.
fn codes_with(changes: serde_json::Value) -> Result<Vec<Disposition>, Box<dyn Error>> {
    let validated = validate_one(&[finding_with(changes)])?;
    Ok(validated.transitions.iter().map(|t| t.code).collect())
}

// wi-641e1a: the schema leaves `rule` and `group_hint` unbounded; validation caps them. The
// texts are cut into short words: a run of 20 characters the tree lacks is an egress hit.
#[test]
fn rejects_a_rule_over_128_bytes_and_keeps_one_at_the_cap() -> Result<(), Box<dyn Error>> {
    let (over, at) = (
        codes_with(json!({ "rule": format!("{}R", "R-".repeat(64)) }))?,
        codes_with(json!({ "rule": "R-".repeat(64) }))?,
    );

    assert_eq!((over, at), (vec![Disposition::Schema], vec![]));
    Ok(())
}

#[test]
fn rejects_a_group_hint_over_100_characters() -> Result<(), Box<dyn Error>> {
    let (over, at) = (
        codes_with(json!({ "group_hint": format!("{}\u{e9}", "\u{e9} ".repeat(50)) }))?,
        codes_with(json!({ "group_hint": "\u{e9} ".repeat(50) }))?,
    );

    assert_eq!((over, at), (vec![Disposition::Schema], vec![]));
    Ok(())
}

// Each finding costs a search of its file, so a document's findings are capped before step 2.
#[test]
fn drops_findings_past_256_in_a_document_as_cap_exceeded() -> Result<(), Box<dyn Error>> {
    let findings = vec![finding(); 257];

    let validated = validate_one(&findings)?;

    assert_eq!(
        (validated.findings.len(), validated.transitions),
        (
            256,
            vec![Transition {
                at: FindingRef::Finding {
                    document: 0,
                    index: 256
                },
                code: Disposition::CapExceeded,
                from: None,
                to: None
            }]
        )
    );
    Ok(())
}

// A role outside the roster has no severity ceiling to hold its findings to.
#[test]
fn disposes_of_a_document_from_a_role_outside_the_roster_as_schema() -> Result<(), Box<dyn Error>> {
    let validated = Run::default().validate(&[document_by("bugs", &[finding()])])?;

    assert_eq!(
        (validated.transitions, validated.findings.len()),
        (
            vec![Transition {
                at: FindingRef::Document { document: 0 },
                code: Disposition::Schema,
                from: None,
                to: None
            }],
            0
        )
    );
    Ok(())
}
