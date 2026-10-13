//! `assay status --local` (#130, 03 §6b): the local check's result as `assay.status/1` with
//! `source: "local"`, read from the report `assay check --local` wrote.
//!
//! It is advisory, as the report is: no PR, no App check, no viewer and nothing waivable. The
//! result is `stale` when it is about another tree than the working tree's fingerprint now, and
//! a status with `check: null` says there is none. The report is read strictly
//! ([`LocalReportError`]) and the document checked against its schema before it is returned.

use std::collections::BTreeMap;

use assay_domain::{Conclusion, Fingerprint};
use serde::Serialize;
use thiserror::Error;

use super::document::{
    DecisionEntry, FindingEntry, TooManyReasons, decision, decision_lines, finding_line, heading,
};
use super::{FetchedAt, MAX_CHECK_RUNS, MAX_FINDINGS, StatusReport};
use crate::check::{LocalReportError, StoredFinding, StoredGate, StoredReport, read_report};
use crate::config::CommitId;
use crate::contract::{Contract, ContractValidator, ReadContractError};

/// What `local_status` is asked.
#[derive(Debug, Clone, Copy)]
pub struct LocalStatusRequest<'a> {
    /// The working tree's fingerprint now (02 §6): its snapshot's git tree id.
    pub tree: &'a str,
    /// The checkout's HEAD, if any.
    pub local_head: Option<&'a CommitId>,
    /// The local report to show: the current tree's when there is one, else the last one
    /// written; `None` when there is none.
    pub report: Option<StoredLocalReport<'a>>,
    /// When the state was read.
    pub fetched_at: &'a FetchedAt,
}

/// A local report's bytes and the tree its file is named for.
#[derive(Debug, Clone, Copy)]
pub struct StoredLocalReport<'a> {
    /// The tree fingerprint the file is named for.
    pub tree: &'a str,
    /// The file's bytes.
    pub bytes: &'a [u8],
}

/// Why no local status could be given. Nothing is printed.
#[derive(Debug, Error)]
pub enum LocalStatusError {
    /// The working tree's fingerprint, or a report file's tree, is not a git tree id.
    #[error("the tree fingerprint is not a git tree id")]
    Tree,
    /// The local report does not read.
    #[error("the local report does not read")]
    Report(#[source] LocalReportError),
    /// The report is about another tree than its file is named for.
    #[error("the local report is about another tree than its file name says")]
    TreeMismatch,
    /// More gate runs than [`MAX_CHECK_RUNS`].
    #[error("{count} gate runs, over the {MAX_CHECK_RUNS} cap")]
    TooManyGates {
        /// How many.
        count: usize,
    },
    /// More findings than [`MAX_FINDINGS`].
    #[error("{count} findings, over the {MAX_FINDINGS} cap")]
    TooManyFindings {
        /// How many.
        count: usize,
    },
    /// More reasons or degraded roles than [`MAX_REASONS`](super::MAX_REASONS).
    #[error("{count} reasons or degraded roles, over the cap")]
    TooManyReasons {
        /// How many.
        count: usize,
    },
    /// The document could not be encoded.
    #[error("could not encode the status")]
    Encode(#[source] serde_json::Error),
    /// The document does not satisfy `status-1.schema.json`: the report holds something the
    /// status cannot carry, such as an empty title.
    #[error("the local status does not satisfy its schema")]
    Invalid(#[source] ReadContractError),
}

impl From<TooManyReasons> for LocalStatusError {
    fn from(over: TooManyReasons) -> Self {
        Self::TooManyReasons { count: over.count }
    }
}

/// The local result `request` names as `assay.status/1`, validated against its schema.
///
/// # Errors
///
/// [`LocalStatusError`] when a tree is not a tree id, the report does not read or is about
/// another tree than its file, a count is over its cap, or the document fails its schema.
pub fn local_status(
    request: &LocalStatusRequest<'_>,
    validator: &ContractValidator,
) -> Result<StatusReport, LocalStatusError> {
    if !is_tree_id(request.tree) {
        return Err(LocalStatusError::Tree);
    }
    let stored = request.report.map(read).transpose()?;
    let document = match &stored {
        Some(stored) => LocalDocument::of(request, stored)?,
        None => LocalDocument::missing(request),
    };
    let mut json = serde_json::to_vec_pretty(&document).map_err(LocalStatusError::Encode)?;
    json.push(b'\n');
    validator
        .read(Contract::Status, &json)
        .map_err(LocalStatusError::Invalid)?;
    Ok(StatusReport {
        text: document.text(),
        json,
    })
}

/// Reads `file`, which must be about the tree it is named for.
fn read(file: StoredLocalReport<'_>) -> Result<StoredReport, LocalStatusError> {
    if !is_tree_id(file.tree) {
        return Err(LocalStatusError::Tree);
    }
    let stored = read_report(file.bytes).map_err(LocalStatusError::Report)?;
    if stored.tree == file.tree {
        Ok(stored)
    } else {
        Err(LocalStatusError::TreeMismatch)
    }
}

/// Whether `text` is a git tree id: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits, as the
/// local report's file is named.
#[must_use]
pub fn is_tree_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `assay.status/1` for the local result, in its schema's field order.
#[derive(Debug, Serialize)]
struct LocalDocument<'a> {
    schema: &'static str,
    source: &'static str,
    local_head_sha: Option<&'a str>,
    tree: Option<&'a str>,
    local_tree: &'a str,
    stale: bool,
    check: Option<LocalCheck>,
    decision: Option<DecisionEntry>,
    tier: Option<&'static str>,
    gates: Vec<LocalGateEntry>,
    findings: Vec<FindingEntry>,
    fetched_at: &'a str,
}

/// The local result as a check would show it; there is no check page.
#[derive(Debug, Serialize)]
struct LocalCheck {
    conclusion: Option<&'static str>,
    status: &'static str,
}

/// One plugin's gates, as a gate check would conclude.
#[derive(Debug, Serialize)]
struct LocalGateEntry {
    plugin: String,
    conclusion: &'static str,
}

impl<'a> LocalDocument<'a> {
    /// No local result.
    fn missing(request: &LocalStatusRequest<'a>) -> Self {
        Self {
            schema: "assay.status/1",
            source: "local",
            local_head_sha: request.local_head.map(CommitId::as_str),
            tree: None,
            local_tree: request.tree,
            stale: false,
            check: None,
            decision: None,
            tier: None,
            gates: Vec::new(),
            findings: Vec::new(),
            fetched_at: request.fetched_at.as_str(),
        }
    }

    /// The result `stored` holds.
    fn of(
        request: &LocalStatusRequest<'a>,
        stored: &'a StoredReport,
    ) -> Result<Self, LocalStatusError> {
        let (conclusion, status) = match stored.decision.conclusion {
            Conclusion::Success => (Some("success"), "completed"),
            Conclusion::Failure => (Some("failure"), "completed"),
            Conclusion::Neutral => (Some("neutral"), "completed"),
            // As `publish` writes a pending decision: its check stays in progress.
            Conclusion::Pending => (None, "in_progress"),
        };
        Ok(Self {
            tree: Some(&stored.tree),
            stale: stored.tree != request.tree,
            check: Some(LocalCheck { conclusion, status }),
            decision: Some(decision(&stored.decision)?),
            tier: Some(stored.tier.code()),
            gates: gates(&stored.gates)?,
            findings: findings(stored)?,
            ..Self::missing(request)
        })
    }

    /// The same facts for a person, one per line, ending in a newline.
    fn text(&self) -> String {
        let mut lines = vec![
            "assay status --local: advisory, CI never reads it".to_owned(),
            format!(
                "Tree: {} (local HEAD {})",
                self.local_tree,
                self.local_head_sha.unwrap_or("unknown")
            ),
            match (self.tree, self.stale) {
                (None, _) => "Local result: none; run assay check --local".to_owned(),
                (Some(_), false) => "Local result: for this tree".to_owned(),
                (Some(tree), true) => format!("Local result: stale, for tree {tree}"),
            },
        ];
        if let Some(check) = &self.check {
            lines.push(format!(
                "Check: {} {}",
                check.status,
                check.conclusion.unwrap_or("-")
            ));
        }
        decision_lines(&mut lines, self.decision.as_ref(), self.tier);
        lines.push(heading("Gates", self.gates.is_empty()));
        lines.extend(
            self.gates
                .iter()
                .map(|gate| format!("  {} {}", gate.plugin, gate.conclusion)),
        );
        lines.push(heading("Findings", self.findings.is_empty()));
        lines.extend(self.findings.iter().map(finding_line));
        lines.push(format!("Fetched: {}", self.fetched_at));
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

/// One entry per plugin, in first-run order, concluding as its worst job; refused past
/// [`MAX_CHECK_RUNS`] runs.
fn gates(stored: &[StoredGate]) -> Result<Vec<LocalGateEntry>, LocalStatusError> {
    if stored.len() > MAX_CHECK_RUNS {
        return Err(LocalStatusError::TooManyGates {
            count: stored.len(),
        });
    }
    let mut worst = Vec::new();
    let mut position: BTreeMap<&str, usize> = BTreeMap::new();
    for gate in stored {
        let conclusion = gate.outcome.check_conclusion();
        if let Some((_, seen)) = position
            .get(gate.name.as_str())
            .and_then(|&at| worst.get_mut(at))
        {
            *seen = conclusion.max(*seen);
        } else {
            position.insert(gate.name.as_str(), worst.len());
            worst.push((gate.name.as_str(), conclusion));
        }
    }
    Ok(worst
        .into_iter()
        .map(|(plugin, conclusion)| LocalGateEntry {
            // The document owns its copy of each plugin name.
            plugin: plugin.to_owned(),
            conclusion: conclusion.code(),
        })
        .collect())
}

/// One entry per fingerprint, gate findings first, each as its most severe finding; refused
/// past [`MAX_FINDINGS`] findings.
fn findings(stored: &StoredReport) -> Result<Vec<FindingEntry>, LocalStatusError> {
    let count = stored
        .gates
        .iter()
        .map(|gate| gate.findings.len())
        .fold(stored.review.len(), usize::saturating_add);
    if count > MAX_FINDINGS {
        return Err(LocalStatusError::TooManyFindings { count });
    }
    let all = stored
        .gates
        .iter()
        .flat_map(|gate| &gate.findings)
        .chain(&stored.review);
    let mut kept: Vec<&StoredFinding> = Vec::new();
    let mut position: BTreeMap<Fingerprint, usize> = BTreeMap::new();
    for finding in all {
        match position.get(&finding.fp).and_then(|&at| kept.get_mut(at)) {
            Some(seen) if finding.severity > seen.severity => *seen = finding,
            Some(_) => {}
            None => {
                position.insert(finding.fp, kept.len());
                kept.push(finding);
            }
        }
    }
    Ok(kept.into_iter().map(FindingEntry::local).collect())
}
