//! A carry set as `assay.run-inputs/1` records it (`docs/architecture/03-contracts.md` §6a,
//! #224): `{findings: [{fp, file, line, end_line?, class, severity, verdict?, title, trigger?,
//! evidence}], omitted: [{fp, reason: cap|refused}]}`. `publish` writes it through
//! [`CarryOut`], borrowing the set; [`prior_carry`](super::prior_carry) reads it back through
//! [`CarryIn`], refusing an unknown field, and hands the findings to the carried intake.

use std::num::NonZeroU64;

use assay_domain::{Class, Fingerprint, Severity, VerifierVerdict};
use serde::{Deserialize, Serialize};

use super::carried::{CarriedFinding, CarriedParts};
use super::carry::{CarrySet, OmitReason, Omitted};

/// The carry set as written, borrowed from the [`CarrySet`].
#[derive(Serialize)]
pub(crate) struct CarryOut<'a> {
    findings: Vec<RecordOut<'a>>,
    omitted: Vec<OmittedRecord>,
}

impl<'a> CarryOut<'a> {
    /// `set` as the bundle records it.
    pub(crate) fn of(set: &'a CarrySet) -> Self {
        Self {
            findings: set.findings().iter().map(RecordOut::of).collect(),
            omitted: set.omitted().iter().map(OmittedRecord::of).collect(),
        }
    }
}

/// One carried finding as written.
#[derive(Serialize)]
struct RecordOut<'a> {
    fp: Fingerprint,
    file: &'a str,
    line: NonZeroU64,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_line: Option<NonZeroU64>,
    class: Class,
    severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<VerifierVerdict>,
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger: Option<&'a str>,
    evidence: &'a str,
}

impl<'a> RecordOut<'a> {
    fn of(finding: &'a CarriedFinding) -> Self {
        Self {
            fp: finding.fp(),
            file: finding.file(),
            line: finding.line(),
            end_line: finding.end_line(),
            class: finding.class(),
            severity: finding.severity(),
            verdict: finding.verdict(),
            title: finding.title(),
            trigger: finding.trigger(),
            evidence: finding.evidence(),
        }
    }
}

/// An open finding left out, as written and read: its fingerprint and why.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OmittedRecord {
    fp: Fingerprint,
    reason: Reason,
}

impl OmittedRecord {
    const fn of(omitted: &Omitted) -> Self {
        Self {
            fp: omitted.fp,
            reason: match omitted.reason {
                OmitReason::Cap => Reason::Cap,
                OmitReason::Refused(_) => Reason::Refused,
            },
        }
    }
}

/// Why a finding was left out, as the contract codes it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Reason {
    Cap,
    Refused,
}

/// The carry set as read, not yet checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CarryIn {
    findings: Vec<RecordIn>,
    /// Checked to be the contract's shape, not kept: nothing reads what was left out.
    #[serde(rename = "omitted")]
    _omitted: Vec<OmittedRecord>,
}

impl CarryIn {
    /// The findings as the carried intake takes them, in the order written.
    pub(crate) fn into_parts(self) -> Vec<CarriedParts> {
        self.findings
            .into_iter()
            .map(RecordIn::into_parts)
            .collect()
    }
}

/// One carried finding as read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIn {
    fp: Fingerprint,
    file: String,
    line: NonZeroU64,
    #[serde(default)]
    end_line: Option<NonZeroU64>,
    class: Class,
    severity: Severity,
    #[serde(default)]
    verdict: Option<VerifierVerdict>,
    title: String,
    #[serde(default)]
    trigger: Option<String>,
    evidence: String,
}

impl RecordIn {
    fn into_parts(self) -> CarriedParts {
        let Self {
            fp,
            file,
            line,
            end_line,
            class,
            severity,
            verdict,
            title,
            trigger,
            evidence,
        } = self;
        CarriedParts {
            fp,
            file,
            line,
            end_line,
            class,
            severity,
            verdict,
            title,
            trigger,
            evidence,
        }
    }
}
