//! #170: a transition names its finding's fingerprint once step 2 has computed one, so a
//! `FindingDisposed` event says which finding it applies to; a transition recorded before that
//! has no fingerprint to name and carries none.

use std::error::Error;

use assay_app::{Contract, ContractValidator, SuppressRules, Validated};
use assay_domain::{Class, Disposition, EventPayload, Finding, Fingerprint};
use serde_json::{Value, json};

use super::fixture::{Run, document, finding, finding_with, validate_one};

/// Suppresses every finding of one class.
#[derive(Debug)]
struct SuppressClass(Class);

impl SuppressRules for SuppressClass {
    fn suppresses(&self, finding: &Finding, _file_at_head: &str) -> bool {
        finding.class == self.0
    }
}

/// The fingerprint `finding` is kept with in the default run.
fn kept_fingerprint(finding: Value) -> Result<Fingerprint, Box<dyn Error>> {
    let validated = validate_one(&[finding])?;
    let kept = validated.findings.first().ok_or("the finding is kept")?;
    Ok(kept.fingerprint)
}

/// Each transition of `validated` as its code and fingerprint.
fn named(validated: &Validated) -> Vec<(Disposition, Option<Fingerprint>)> {
    validated
        .transitions
        .iter()
        .map(|transition| (transition.code, transition.fp))
        .collect()
}

#[test]
fn a_severity_cap_names_the_kept_finding_s_fingerprint() -> Result<(), Box<dyn Error>> {
    let validated = validate_one(&[finding_with(json!({ "trigger": null }))])?;

    let kept = validated.findings.first().ok_or("the finding is kept")?;
    assert_eq!(
        named(&validated),
        [(Disposition::NoTrigger, Some(kept.fingerprint))]
    );
    Ok(())
}

#[test]
fn a_move_to_the_summary_names_the_kept_finding_s_fingerprint() -> Result<(), Box<dyn Error>> {
    // Line 7 is in `other`, outside the only hunk.
    let validated = validate_one(&[finding_with(json!({ "line": 7 }))])?;

    let kept = validated.findings.first().ok_or("the finding is kept")?;
    assert_eq!(
        named(&validated),
        [(Disposition::LineNotInDiffMoved, Some(kept.fingerprint))]
    );
    Ok(())
}

// A finding dropped after step 2 names the fingerprint it would have been kept with.
#[test]
fn a_drop_after_anchoring_names_the_finding_s_fingerprint() -> Result<(), Box<dyn Error>> {
    let rules = SuppressClass(Class::ErrorHandling);
    let run = Run {
        suppress: &rules,
        ..Run::default()
    };

    let validated = run.validate(&[document(&[finding()])])?;

    assert_eq!(
        named(&validated),
        [(
            Disposition::SuppressedByRule,
            Some(kept_fingerprint(finding())?)
        )]
    );
    Ok(())
}

// The injection screen (step 5) runs after step 2, so its drop names the fingerprint too.
#[test]
fn an_injection_suspect_after_anchoring_names_the_finding_s_fingerprint()
-> Result<(), Box<dyn Error>> {
    let suspect = finding_with(json!({ "body": "cc @owner please look" }));

    let validated = validate_one(&[suspect])?;

    let clean = finding_with(json!({ "body": "please look" }));
    assert_eq!(
        named(&validated),
        [(
            Disposition::InjectionSuspect,
            Some(kept_fingerprint(clean)?)
        )]
    );
    Ok(())
}

// Before step 2 there is no fingerprint (03 §3 hashes the located evidence), and none is made up:
// not for a document refused whole, a finding past the cap or one the egress scan drops.
#[test]
fn a_transition_before_anchoring_names_no_fingerprint() -> Result<(), Box<dyn Error>> {
    let mut findings = vec![finding(); 257];
    findings.push(finding_with(
        json!({ "body": "Zq3Lw9Xv2RtHp8NcYb4MdKs7Ue1Ja6Gf0Ti5Wo" }),
    ));

    let validated = Run::default().validate(&[b"[]".to_vec(), document(&findings)])?;

    assert_eq!(
        named(&validated),
        [
            (Disposition::Schema, None),
            (Disposition::CapExceeded, None),
            (Disposition::InjectionSuspect, None),
        ]
    );
    Ok(())
}

#[test]
fn a_finding_level_drop_before_anchoring_names_no_fingerprint() -> Result<(), Box<dyn Error>> {
    let missing = finding_with(json!({ "evidence": "let y = compute(2);" }));
    let unshaped = finding_with(json!({ "rule": format!("{}R", "R-".repeat(64)) }));

    let validated = validate_one(&[missing, unshaped])?;

    assert_eq!(
        named(&validated),
        [
            (Disposition::EvidenceMissing, None),
            (Disposition::Schema, None)
        ]
    );
    Ok(())
}

/// `payload` in an `assay.event/1` envelope, as JSON bytes.
fn enveloped(payload: &EventPayload) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut event = json!({
        "schema": "assay.event/1",
        "job": "review",
        "idx": 0,
        "ts": "2026-10-14T09:00:00Z",
        "run_key": "run-42-1",
        "run_uid": { "repo_id": 840_213_377, "run_id": 7, "run_attempt": 1 },
        "stage": "S4",
    });
    if let (Some(fields), Value::Object(tagged)) =
        (event.as_object_mut(), serde_json::to_value(payload)?)
    {
        fields.extend(tagged);
    }
    Ok(serde_json::to_vec(&event)?)
}

#[test]
fn each_event_names_its_transition_s_fingerprint_and_is_a_valid_event() -> Result<(), Box<dyn Error>>
{
    let validated = Run::default().validate(&[
        b"[]".to_vec(),
        document(&[finding_with(json!({ "trigger": null }))]),
    ])?;
    let kept = validated.findings.first().ok_or("the finding is kept")?;

    let events: Vec<_> = validated.events().collect();

    assert_eq!(
        events,
        [
            EventPayload::FindingDisposed {
                code: Disposition::Schema,
                fp: None,
                from: None,
                to: None,
            },
            EventPayload::FindingDisposed {
                code: Disposition::NoTrigger,
                fp: Some(kept.fingerprint),
                from: Some(assay_domain::Severity::Major),
                to: Some(assay_domain::Severity::Minor),
            },
        ]
    );
    let validator = ContractValidator::new()?;
    for event in &events {
        validator.read(Contract::Event, &enveloped(event)?)?;
    }
    Ok(())
}
