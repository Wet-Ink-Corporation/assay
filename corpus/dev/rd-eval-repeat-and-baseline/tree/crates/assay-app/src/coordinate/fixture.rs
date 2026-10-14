//! Validated findings for the coordinator's unit tests.

use std::num::NonZeroU64;

use assay_domain::{
    Class, Finding, FindingKind, Fingerprint, ParseFingerprintError, Severity, Side,
};

use crate::validation::{FindingRef, Placement, ValidFinding};

/// An inline `logic` finding of `severity` on `src/lib.rs`, line 3, with fingerprint `fp`.
pub(super) fn valid(fp: u64, severity: Severity) -> Result<ValidFinding, ParseFingerprintError> {
    Ok(ValidFinding {
        at: FindingRef::Finding {
            document: 0,
            index: 0,
        },
        finding: Finding {
            class: Class::Logic,
            rule: None,
            kind: FindingKind::Defect,
            severity,
            introduced_by: None,
            pre_existing: false,
            file: "src/lib.rs".to_owned(),
            line: NonZeroU64::MIN.saturating_add(2),
            end_line: None,
            side: Some(Side::Right),
            evidence: "record_event(id);".to_owned(),
            expected_behavior: None,
            trigger: None,
            caller: None,
            title: "Event recorded twice".to_owned(),
            body: "The retry path records the event again.".to_owned(),
            fix: None,
            confidence: None,
            group_hint: None,
        },
        file_at_head: "src/lib.rs".to_owned(),
        enclosing_symbol: "f".to_owned(),
        occurrence_index: 0,
        fingerprint: format!("{fp:016x}").parse::<Fingerprint>()?,
        placement: Placement::Inline,
        previously_dismissed: None,
    })
}

/// [`valid`] of `class`.
pub(super) fn of_class(
    fp: u64,
    severity: Severity,
    class: Class,
) -> Result<ValidFinding, ParseFingerprintError> {
    let mut found = valid(fp, severity)?;
    found.finding.class = class;
    Ok(found)
}
