//! Step 3: inline only on a hunk line, interactions need a real caller, and `pre_existing` is
//! honoured only outside the diff and not for an interaction.

use std::error::Error;

use assay_app::{FindingRef, Placement, Validated};
use assay_domain::Disposition;
use serde_json::{Value, json};

use super::fixture::{Run, document, finding_with, validate_one};

/// Line 7 of `src/lib.rs` is in `other`, outside the only hunk (lines 1 to 5).
fn outside_the_diff(extra: Value) -> Value {
    let mut changes = json!({ "line": 7 });
    if let (Some(fields), Value::Object(extra)) = (changes.as_object_mut(), extra) {
        fields.extend(extra);
    }
    finding_with(changes)
}

fn codes(validated: &Validated) -> Vec<Disposition> {
    validated.transitions.iter().map(|t| t.code).collect()
}

#[test]
fn moves_a_finding_outside_every_hunk_to_the_summary_and_keeps_it() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[outside_the_diff(json!({}))])?;

    let kept: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| (kept.finding.line.get(), kept.placement))
        .collect();
    assert_eq!(
        (kept, codes(&validated)),
        (
            vec![(7, Placement::Summary)],
            vec![Disposition::LineNotInDiffMoved]
        )
    );
    Ok(())
}

#[test]
fn honours_pre_existing_outside_the_diff() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[outside_the_diff(json!({ "pre_existing": true }))])?;

    let flags: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.pre_existing)
        .collect();
    assert_eq!(
        (flags, codes(&validated)),
        (
            vec![true],
            vec![Disposition::LineNotInDiffMoved, Disposition::PreExisting]
        )
    );
    Ok(())
}

#[test]
fn resets_pre_existing_on_a_line_in_the_diff() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(json!({ "pre_existing": true }))])?;

    let flags: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| kept.finding.pre_existing)
        .collect();
    assert_eq!((flags, codes(&validated)), (vec![false], vec![]));
    Ok(())
}

/// The default run with `src/handler.rs` calling `parse`, unchanged.
fn with_caller() -> Run<'static> {
    let mut run = Run::default();
    run.change.head_files.insert(
        "src/handler.rs".to_owned(),
        "fn handle(body: &str) -> u32 {\n    parse(&body)?\n}\n".to_owned(),
    );
    run
}

fn interaction(caller_line: u64, caller_evidence: &str, extra: Value) -> Value {
    let mut changes = json!({
        "introduced_by": "interaction",
        "caller": { "file": "src/handler.rs", "line": caller_line, "evidence": caller_evidence },
    });
    if let (Some(fields), Value::Object(extra)) = (changes.as_object_mut(), extra) {
        fields.extend(extra);
    }
    finding_with(changes)
}

#[test]
fn keeps_an_interaction_and_relocates_its_caller() -> Result<(), Box<dyn Error>> {
    let validated =
        with_caller().validate(&[document(&[interaction(9, "parse(&body)?", json!({}))])])?;

    let callers: Vec<_> = validated
        .findings
        .iter()
        .filter_map(|kept| kept.finding.caller.as_ref())
        .map(|caller| caller.line.get())
        .collect();
    assert_eq!((callers, codes(&validated)), (vec![2], vec![]));
    Ok(())
}

#[test]
fn replaces_the_caller_s_evidence_with_the_tree_s_line() -> Result<(), Box<dyn Error>> {
    let claim = interaction(2, "parse(&body)? // the call", json!({}));

    let validated = with_caller().validate(&[document(&[claim])])?;

    let callers: Vec<_> = validated
        .findings
        .iter()
        .filter_map(|kept| kept.finding.caller.as_ref())
        .map(|caller| caller.evidence.as_str())
        .collect();
    assert_eq!(callers, ["    parse(&body)?"]);
    Ok(())
}

// wi-d4735c: the caller evidence the model wrote is screened; the tree's line that replaces it is
// repository content, published as code, and is not.
#[test]
fn screens_the_model_s_caller_text_and_not_the_tree_s_line() -> Result<(), Box<dyn Error>> {
    let mut run = Run::default();
    run.change.head_files.insert(
        "src/handler.rs".to_owned(),
        concat!(
            "fn handle(body: &str) -> u32 {\n",
            "    parse(&body)?;\n",
            "    let v: Vec<u8> = decode(&body)?;\n",
            "}\n",
        )
        .to_owned(),
    );
    let claims = [
        interaction(2, "parse(&body)?; // cc @owner", json!({})),
        // The quote elides the generic, so only the tree's line holds `Vec<u8>`.
        interaction(3, "let v: Vec ... = decode(&body)?;", json!({})),
    ];

    let validated = run.validate(&[document(&claims)])?;

    let kept: Vec<_> = validated
        .findings
        .iter()
        .filter_map(|kept| kept.finding.caller.as_ref())
        .map(|caller| caller.evidence.as_str())
        .collect();
    assert_eq!(
        (kept, codes(&validated)),
        (
            vec!["    let v: Vec<u8> = decode(&body)?;"],
            vec![Disposition::InjectionSuspect]
        )
    );
    Ok(())
}

// wi-fd9898: an interaction whose changed line is outside every hunk is not dropped for it; it
// stays in the summary, where it still counts.
#[test]
fn keeps_an_interaction_outside_the_diff_in_the_summary() -> Result<(), Box<dyn Error>> {
    let claim = interaction(2, "parse(&body)?", json!({ "line": 7 }));

    let validated = with_caller().validate(&[document(&[claim])])?;

    let kept: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| (kept.finding.line.get(), kept.placement))
        .collect();
    assert_eq!(
        (kept, codes(&validated)),
        (
            vec![(7, Placement::Summary)],
            vec![Disposition::LineNotInDiffMoved]
        )
    );
    Ok(())
}

#[test]
fn drops_an_interaction_whose_caller_evidence_is_missing() -> Result<(), Box<dyn Error>> {
    let validated =
        with_caller().validate(&[document(&[interaction(2, "parse(&other)?", json!({}))])])?;

    assert_eq!(
        (validated.findings.len(), codes(&validated)),
        (0, vec![Disposition::EvidenceMissing])
    );
    Ok(())
}

#[test]
fn resets_pre_existing_on_an_interaction_even_outside_the_diff() -> Result<(), Box<dyn Error>> {
    let claim = interaction(
        2,
        "parse(&body)?",
        json!({ "line": 7, "pre_existing": true }),
    );

    let validated = with_caller().validate(&[document(&[claim])])?;

    let kept: Vec<_> = validated
        .findings
        .iter()
        .map(|kept| (kept.at, kept.placement, kept.finding.pre_existing))
        .collect();
    assert_eq!(
        (kept, codes(&validated)),
        (
            vec![(
                FindingRef::Finding {
                    document: 0,
                    index: 0
                },
                Placement::Summary,
                false
            )],
            vec![Disposition::LineNotInDiffMoved]
        )
    );
    Ok(())
}
