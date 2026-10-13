//! The `assay.status/1` document and its plain-text rendering.

use std::num::NonZeroU64;

use assay_domain::{Class, Decision, Disposition, WeighIn};
use serde::Serialize;

use super::findings::Finding;
use super::ports::{ObservedConclusion, PullView, Viewer};
use super::verdict::{Check, Observed};
use super::{MAX_REASONS, StatusError, StatusRequest, StatusUnreadable};
use crate::CommitId;
use crate::check::StoredFinding;
use crate::publish::{CheckStatus, clean, is_hidden};

/// The longest GitHub login, before a `[bot]` suffix.
const MAX_LOGIN_BYTES: usize = 39;

/// `assay.status/1`, in its schema's field order.
#[derive(Debug, Serialize)]
pub(super) struct Document<'a> {
    schema: &'static str,
    source: &'static str,
    repo: &'a str,
    pr: NonZeroU64,
    branch: &'a str,
    head_sha: &'a str,
    local_head_sha: Option<&'a str>,
    stale: bool,
    check: Option<CheckEntry>,
    decision: Option<DecisionEntry>,
    tier: Option<&'static str>,
    gates: Vec<GateEntry>,
    findings: Vec<FindingEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    weigh_in: Option<WeighInEntry>,
    viewer: ViewerEntry<'a>,
    fetched_at: &'a str,
}

#[derive(Debug, Serialize)]
struct CheckEntry {
    conclusion: Option<&'static str>,
    status: &'static str,
    url: String,
}

#[derive(Debug, Serialize)]
pub(super) struct DecisionEntry {
    conclusion: &'static str,
    reasons: Vec<ReasonEntry>,
    reasons_digest: String,
    counts: assay_domain::Counts,
    degraded: Vec<DegradedEntry>,
    advisory_blockers: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ReasonEntry {
    code: &'static str,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fp: Option<String>,
}

#[derive(Debug, Serialize)]
struct DegradedEntry {
    role: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct GateEntry {
    plugin: String,
    conclusion: Option<&'static str>,
    url: String,
}

#[derive(Debug, Serialize)]
pub(super) struct FindingEntry {
    fp: String,
    finding_id: String,
    severity: &'static str,
    class: Option<&'static str>,
    title: Option<String>,
    file: Option<String>,
    line: Option<NonZeroU64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disposition: Option<&'static str>,
    waivable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

/// What the required Weigh-In gate reported, as the domain's code: `clear`, `open-blocking`,
/// `pending-triage` or `no-evidence` (one state, so no two can be claimed at once).
#[derive(Debug, Serialize)]
struct WeighInEntry {
    state: &'static str,
}

#[derive(Debug, Serialize)]
struct ViewerEntry<'a> {
    login: &'a str,
    is_owner: bool,
    is_agent: bool,
}

/// What was read for one PR, the document's facts besides the request.
pub(super) struct Facts<'a> {
    /// The PR.
    pub(super) pull: &'a PullView,
    /// Its checks, and the decision the verdict check names.
    pub(super) observed: Observed,
    /// The decision's findings.
    pub(super) findings: Vec<Finding>,
    /// Who is asking.
    pub(super) viewer: &'a Viewer,
}

impl<'a> Document<'a> {
    /// The document for `facts.pull`, from what was observed and read.
    pub(super) fn new<E, B>(
        request: &StatusRequest<'a>,
        facts: Facts<'a>,
    ) -> Result<Self, StatusError<E, B>> {
        let Facts {
            pull,
            observed,
            findings,
            viewer,
        } = facts;
        let head_sha = pull.head_sha.as_str();
        let local_head_sha = request.local_head.map(CommitId::as_str);
        let (decision, tier, weigh_in) = match &observed.decided {
            Some(decided) => (
                Some(
                    decision(&decided.decision)
                        .map_err(|over| StatusError::TooManyReasons { count: over.count })?,
                ),
                Some(decided.inputs.pr.tier.code()),
                weigh_in(decided.inputs.weigh_in),
            ),
            None => (None, None, None),
        };
        Ok(Self {
            schema: "assay.status/1",
            source: "ci",
            repo: &request.repository.name,
            pr: pull.number,
            branch: &pull.head_ref,
            head_sha,
            local_head_sha,
            stale: local_head_sha.is_some_and(|local| local != head_sha),
            check: observed.check.map(check_entry),
            decision,
            tier,
            gates: observed
                .gates
                .into_iter()
                .map(|(plugin, check)| GateEntry {
                    plugin: plugin.as_str().to_owned(),
                    conclusion: check.conclusion.map(ObservedConclusion::code),
                    url: check.url,
                })
                .collect(),
            findings: findings.into_iter().map(finding_entry).collect(),
            weigh_in,
            viewer: viewer_entry(request, viewer).map_err(StatusError::Unreadable)?,
            fetched_at: request.fetched_at.as_str(),
        })
    }

    /// The same facts for a person, one per line, ending in a newline.
    pub(super) fn text(&self) -> String {
        let local = match self.local_head_sha {
            None => "unknown",
            Some(_) if self.stale => "differs: this status is for another commit",
            Some(_) => "matches",
        };
        let mut lines = vec![
            format!("assay status: {} #{} ({})", self.repo, self.pr, self.branch),
            format!("Head: {} (local HEAD {local})", self.head_sha),
            match &self.check {
                None => "Check: none yet".to_owned(),
                Some(check) => format!(
                    "Check: {} {} {}",
                    check.status,
                    check.conclusion.unwrap_or("-"),
                    check.url
                ),
            },
        ];
        self.decision_lines(&mut lines);
        lines.push(heading("Gates", self.gates.is_empty()));
        lines.extend(self.gates.iter().map(|gate| {
            format!(
                "  {} {} {}",
                gate.plugin,
                gate.conclusion.unwrap_or("-"),
                gate.url
            )
        }));
        lines.push(heading("Findings", self.findings.is_empty()));
        for finding in &self.findings {
            lines.push(finding_line(finding));
            if let Some(body) = &finding.body {
                body_lines(&mut lines, body);
            }
        }
        if let Some(weigh_in) = &self.weigh_in {
            lines.push(format!("Weigh-In: {}", weigh_in.state));
        }
        let role = match (self.viewer.is_owner, self.viewer.is_agent) {
            (true, _) => "owner",
            (false, true) => "agent",
            (false, false) => "other",
        };
        lines.push(format!("Viewer: {} ({role})", self.viewer.login));
        lines.push(format!("Fetched: {}", self.fetched_at));
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }

    fn decision_lines(&self, lines: &mut Vec<String>) {
        decision_lines(lines, self.decision.as_ref(), self.tier);
    }
}

/// The decision's lines, or `Decision: none`.
pub(super) fn decision_lines(
    lines: &mut Vec<String>,
    decision: Option<&DecisionEntry>,
    tier: Option<&str>,
) {
    let Some(decision) = decision else {
        lines.push("Decision: none".to_owned());
        return;
    };
    lines.push(format!(
        "Decision: {} (tier {})",
        decision.conclusion,
        tier.unwrap_or("-")
    ));
    lines.extend(decision.reasons.iter().map(|reason| match &reason.fp {
        Some(fp) => format!("  reason {}: {} ({fp})", reason.code, reason.detail),
        None => format!("  reason {}: {}", reason.code, reason.detail),
    }));
    let counts = decision.counts;
    lines.push(format!(
        "  counts: {} blocker, {} major, {} minor, {} nit",
        counts.blocker, counts.major, counts.minor, counts.nit
    ));
    lines.extend(
        decision
            .degraded
            .iter()
            .map(|degraded| format!("  degraded {}: {}", degraded.role, degraded.reason)),
    );
    lines.extend(
        decision
            .advisory_blockers
            .iter()
            .map(|fp| format!("  advisory blocker {fp}")),
    );
    lines.push(format!("  reasons digest: {}", decision.reasons_digest));
}

/// `Name:`, or `Name: none` for an empty list.
pub(super) fn heading(name: &str, empty: bool) -> String {
    if empty {
        format!("{name}: none")
    } else {
        format!("{name}:")
    }
}

/// One finding on one line: fingerprint, severity, whether it is waivable, then what was
/// published (class, place, title) where it was. The title is cleaned onto one line.
pub(super) fn finding_line(finding: &FindingEntry) -> String {
    let mut parts = vec![
        "  ".to_owned() + finding.fp.as_str(),
        finding.severity.to_owned(),
    ];
    if finding.waivable {
        parts.push("waivable".to_owned());
    }
    if let Some(class) = finding.class {
        parts.push(class.to_owned());
    }
    if let (Some(file), Some(line)) = (&finding.file, finding.line) {
        parts.push(format!("{file}:{line}"));
    }
    if let Some(title) = &finding.title {
        parts.push(clean(title));
    }
    parts.join(" ")
}

/// A `--full` body under its finding, each line indented six spaces with every control and
/// format character (escape sequences and carriage returns included) shown as U+FFFD, so the
/// published text cannot drive the terminal; a blank line stays blank. Not cut: the body is
/// already capped at `MAX_BODY_CHARS`.
fn body_lines(lines: &mut Vec<String>, body: &str) {
    lines.extend(body.split('\n').map(|line| {
        if line.is_empty() {
            String::new()
        } else {
            let shown: String = line
                .chars()
                .map(|c| if is_hidden(c) { '\u{fffd}' } else { c })
                .collect();
            ["      ", &shown].concat()
        }
    }));
}

fn check_entry(check: Check) -> CheckEntry {
    CheckEntry {
        conclusion: check.conclusion.map(ObservedConclusion::code),
        status: match check.status {
            CheckStatus::Queued => "queued",
            CheckStatus::InProgress => "in_progress",
            CheckStatus::Completed => "completed",
        },
        url: check.url,
    }
}

/// More reasons or degraded roles than [`MAX_REASONS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TooManyReasons {
    /// The larger of the two counts.
    pub(super) count: usize,
}

/// The decision as published: reason details and degraded roles cleaned as the summary cleans
/// them; refused past [`MAX_REASONS`].
pub(super) fn decision(decision: &Decision) -> Result<DecisionEntry, TooManyReasons> {
    let count = decision.reasons.len().max(decision.degraded.len());
    if count > MAX_REASONS {
        return Err(TooManyReasons { count });
    }
    Ok(DecisionEntry {
        conclusion: decision.conclusion.code(),
        reasons: decision
            .reasons
            .iter()
            .map(|reason| ReasonEntry {
                code: reason.code.code(),
                detail: clean(&reason.detail),
                fp: reason.fp.map(|fp| fp.to_string()),
            })
            .collect(),
        reasons_digest: decision.reasons_digest(),
        counts: decision.counts,
        degraded: decision
            .degraded
            .iter()
            .map(|degraded| DegradedEntry {
                role: clean(&degraded.role),
                reason: clean(&degraded.reason),
            })
            .collect(),
        advisory_blockers: decision
            .advisory_blockers
            .iter()
            .map(ToString::to_string)
            .collect(),
    })
}

/// The Weigh-In entry: none when the gate is not required, else its state by the domain's code.
fn weigh_in(weigh_in: WeighIn) -> Option<WeighInEntry> {
    match weigh_in {
        WeighIn::NotRequired => None,
        WeighIn::Clear | WeighIn::OpenBlocking | WeighIn::PendingTriage | WeighIn::NoEvidence => {
            Some(WeighInEntry {
                state: weigh_in.code(),
            })
        }
    }
}

impl FindingEntry {
    /// A local finding: its fingerprint, severity and class, with its title and file cleaned
    /// onto one line as the terminal report shows them. Never waivable: there is no PR.
    pub(super) fn local(finding: &StoredFinding) -> Self {
        let fp = finding.fp.to_string();
        Self {
            // The id `/assay waive` takes is the fingerprint: the document carries both fields.
            finding_id: fp.clone(),
            fp,
            severity: finding.severity.code(),
            class: Some(finding.class.code()),
            title: Some(clean(&finding.title)),
            file: Some(clean(&finding.file)),
            line: Some(finding.line),
            url: None,
            disposition: None,
            waivable: false,
            body: None,
        }
    }
}

fn finding_entry(finding: Finding) -> FindingEntry {
    let fp = finding.fp.to_string();
    let (class, title, file, line, url, body) = match finding.published {
        Some(published) => (
            published.class.map(Class::code),
            published.title,
            Some(published.file),
            published.line,
            Some(published.url),
            published.body,
        ),
        None => (None, None, None, None, None, None),
    };
    FindingEntry {
        // The id `/assay waive` takes is the fingerprint: the document carries both fields.
        finding_id: fp.clone(),
        fp,
        severity: finding.severity.code(),
        class,
        title: title.or(finding.gate_title),
        file,
        line,
        url,
        disposition: finding.disposition.map(Disposition::code),
        waivable: finding.waivable,
        body,
    }
}

/// The viewer, when their login reads as a GitHub login (`[bot]` allowed), with whether their
/// id is in the privileged set and in `.assay/agents`.
fn viewer_entry<'a>(
    request: &StatusRequest<'_>,
    viewer: &'a Viewer,
) -> Result<ViewerEntry<'a>, StatusUnreadable> {
    let login = viewer.login.as_str();
    let name = login.strip_suffix("[bot]").unwrap_or(login);
    let readable = name.len() <= MAX_LOGIN_BYTES
        && name
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !readable {
        return Err(StatusUnreadable::Login);
    }
    Ok(ViewerEntry {
        login,
        is_owner: request.identities.privileged.contains(&viewer.id),
        is_agent: request.identities.agents.contains(&viewer.id),
    })
}
