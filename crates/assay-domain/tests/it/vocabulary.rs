//! Every closed vocabulary of 03 has exactly the wire codes its schema and the specification
//! list: a new variant without a schema change, or a schema value without a variant, fails here.

use std::collections::BTreeSet;
use std::error::Error;

use assay_domain::{
    Class, Confidence, Disposition, EventKind, FeedbackSignal, FindingKind, GateConclusion,
    IntroducedBy, RateLimitStatus, ReasonCode, Severity, Side, SourceKind, VerifierVerdict,
    VoidReason,
};

use crate::support::{enum_at, schema, spec_line, spec_names, wire_codes};

/// The backticked names of the 03 §5 line starting `prefix`: after its `(closed enum, ...): `
/// preamble and before the first parenthesised remark (`` `no-trigger` (capped at `minor` ``).
fn spec_list(prefix: &str) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let line = spec_line(prefix)?;
    let (_, list) = line.split_once("): ").ok_or("03 §5 line has no list")?;
    let list = list.split_once(" (").map_or(list, |(list, _)| list);
    Ok(spec_names(list))
}

#[test]
fn reason_codes_are_exactly_the_decision_schema_enum() -> Result<(), Box<dyn Error>> {
    let schema = enum_at(&schema("decision-1")?, "/$defs/reasonCode/enum")?;

    let domain = wire_codes(ReasonCode::ALL)?;

    assert_eq!(domain, schema);
    Ok(())
}

#[test]
fn reason_codes_are_exactly_the_spec_list() -> Result<(), Box<dyn Error>> {
    let spec = spec_list("Reason codes (closed enum")?;

    let domain = wire_codes(ReasonCode::ALL)?;

    assert_eq!(domain, spec);
    Ok(())
}

#[test]
fn reason_code_serialises_as_its_code() -> Result<(), Box<dyn Error>> {
    for reason in ReasonCode::ALL {
        let wire = serde_json::to_value(reason)?;

        assert_eq!(wire, reason.code(), "{reason:?}");
    }
    Ok(())
}

#[test]
fn disposition_codes_are_exactly_the_spec_list() -> Result<(), Box<dyn Error>> {
    let spec = spec_list("Disposition codes (closed enum")?;

    let domain = wire_codes(Disposition::ALL)?;

    assert_eq!(domain, spec);
    Ok(())
}

/// The finding vocabularies of 03 §3 against `findings-1.schema.json`.
#[test]
fn finding_vocabularies_are_exactly_the_findings_schema_enums() -> Result<(), Box<dyn Error>> {
    let findings = schema("findings-1")?;
    let field = |name: &str| format!("/$defs/finding/properties/{name}/enum");

    let pairs = [
        (wire_codes(Class::ALL)?, field("class")),
        (wire_codes(FindingKind::ALL)?, field("kind")),
        (wire_codes(Severity::ALL)?, field("severity")),
        (wire_codes(IntroducedBy::ALL)?, field("introduced_by")),
        (wire_codes(Side::ALL)?, field("side")),
        (wire_codes(Confidence::ALL)?, field("confidence")),
        (
            wire_codes(SourceKind::ALL)?,
            "/$defs/source/properties/kind/enum".to_owned(),
        ),
        (
            wire_codes(GateConclusion::ALL)?,
            "/properties/conclusion/enum".to_owned(),
        ),
    ];

    for (domain, pointer) in pairs {
        assert_eq!(domain, enum_at(&findings, &pointer)?, "{pointer}");
    }
    Ok(())
}

/// The event vocabularies of 03 §6 against `event-1.schema.json`.
#[test]
fn event_vocabularies_are_exactly_the_event_schema_enums() -> Result<(), Box<dyn Error>> {
    let event = schema("event-1")?;
    let data = |when: &str, name: &str| {
        format!("/$defs/{when}/then/properties/data/properties/{name}/enum")
    };

    let pairs = [
        (
            wire_codes(EventKind::ALL)?,
            "/properties/kind/enum".to_owned(),
        ),
        (
            wire_codes(GateConclusion::ALL)?,
            data("whenGate", "conclusion"),
        ),
        (
            wire_codes(RateLimitStatus::ALL)?,
            data("whenRateLimitObserved", "status"),
        ),
        (
            wire_codes(VerifierVerdict::ALL)?,
            data("whenFindingVerified", "verdict"),
        ),
        (
            wire_codes(Disposition::ALL)?,
            data("whenFindingDisposed", "code"),
        ),
        (
            wire_codes(FeedbackSignal::ALL)?,
            data("whenFeedbackReceived", "signal"),
        ),
        (
            wire_codes(VoidReason::ALL)?,
            data("whenCommandVoided", "reason"),
        ),
        (
            wire_codes(Severity::ALL)?,
            "/$defs/severity/enum".to_owned(),
        ),
    ];

    for (domain, pointer) in pairs {
        assert_eq!(domain, enum_at(&event, &pointer)?, "{pointer}");
    }
    Ok(())
}

#[test]
fn event_kinds_are_exactly_the_spec_list() -> Result<(), Box<dyn Error>> {
    let spec = spec_names(&spec_line("`RunRequested`, `PlanComputed`")?);

    let domain = wire_codes(EventKind::ALL)?;

    assert_eq!(domain, spec);
    Ok(())
}

#[test]
fn disposition_serialises_as_its_code() -> Result<(), Box<dyn Error>> {
    for disposition in Disposition::ALL {
        let wire = serde_json::to_value(disposition)?;

        assert_eq!(wire, disposition.code(), "{disposition:?}");
    }
    Ok(())
}
