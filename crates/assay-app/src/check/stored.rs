//! A local report read back from its file for `assay status --local` (#130).
//!
//! The file is local mode's own, in the developer's git directory, but anything there may have
//! been edited, so it is read as strictly as assay writes it (wi-b5affa): at most
//! [`MAX_DOCUMENT_BYTES`], no repeated key, no unknown field outside a finding, the local
//! report's schema name and `advisory: true`, and each outcome, status, tier, severity and
//! fingerprint in its closed form. A finding's fields the status does not show are not read.

use std::num::NonZeroU64;

use assay_domain::{Class, Decision, Fingerprint, Severity, Tier};
use serde::Deserialize;
use thiserror::Error;

use super::outcome::LocalOutcome;
use super::report::{ReviewStatus, SCHEMA};
use crate::contract::{MAX_DOCUMENT_BYTES, parse_json_strict};

/// Why a stored local report was not read.
#[derive(Debug, Error)]
pub enum LocalReportError {
    /// The file is over [`MAX_DOCUMENT_BYTES`].
    #[error("the local report is {size} bytes, over the {MAX_DOCUMENT_BYTES}-byte cap")]
    TooLarge {
        /// Its size.
        size: usize,
    },
    /// It is not JSON, repeats a key, or does not have the local report's shape.
    #[error("the local report does not read")]
    Malformed(#[source] serde_json::Error),
    /// It names another schema, or is not advisory.
    #[error("the file is not an advisory local report")]
    NotLocal,
}

/// The parts of a stored report the local status shows.
#[derive(Debug)]
pub(crate) struct StoredReport {
    /// The tree fingerprint it is about.
    pub(crate) tree: String,
    pub(crate) tier: Tier,
    /// Every gate, in job then file order.
    pub(crate) gates: Vec<StoredGate>,
    /// The review's findings.
    pub(crate) review: Vec<StoredFinding>,
    pub(crate) decision: Decision,
}

/// The report as written; every field is read so an unknown one is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema: String,
    advisory: bool,
    tree: String,
    #[serde(rename = "base")]
    _base: String,
    #[serde(rename = "head")]
    _head: String,
    tier: Tier,
    gates: Vec<StoredGate>,
    review: StoredReview,
    decision: Decision,
}

/// One gate as the report writes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredGate {
    pub(crate) name: String,
    #[serde(rename = "job")]
    _job: String,
    #[serde(rename = "required")]
    _required: bool,
    pub(crate) outcome: LocalOutcome,
    #[serde(default, rename = "cause")]
    _cause: Option<String>,
    pub(crate) findings: Vec<StoredFinding>,
}

/// The review as the report writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReview {
    #[serde(rename = "status")]
    _status: ReviewStatus,
    #[serde(rename = "role")]
    _role: Option<String>,
    #[serde(default, rename = "note")]
    _note: Option<String>,
    findings: Vec<StoredFinding>,
    #[serde(rename = "cost")]
    _cost: String,
}

/// A finding's fingerprint and the fields the status shows.
#[derive(Debug, Deserialize)]
pub(crate) struct StoredFinding {
    pub(crate) fp: Fingerprint,
    pub(crate) class: Class,
    pub(crate) severity: Severity,
    pub(crate) file: String,
    pub(crate) line: NonZeroU64,
    pub(crate) title: String,
}

/// Reads `bytes` as a local report.
///
/// # Errors
///
/// [`LocalReportError`] when it is over the cap, does not parse strictly into the report's
/// shape, or is not an advisory local report.
pub(crate) fn read(bytes: &[u8]) -> Result<StoredReport, LocalReportError> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(LocalReportError::TooLarge { size: bytes.len() });
    }
    let value = parse_json_strict(bytes).map_err(LocalReportError::Malformed)?;
    let stored: Stored = serde_json::from_value(value).map_err(LocalReportError::Malformed)?;
    if stored.schema != SCHEMA || !stored.advisory {
        return Err(LocalReportError::NotLocal);
    }
    Ok(StoredReport {
        tree: stored.tree,
        tier: stored.tier,
        gates: stored.gates,
        review: stored.review.findings,
        decision: stored.decision,
    })
}
