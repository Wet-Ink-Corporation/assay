//! The local report: what `assay check --local` writes to `.git/assay/<tree>.json` and shows in
//! the terminal.
//!
//! It is not a contract. Its `schema` is `assay.local-check/1`, which no schema in `schemas/`
//! describes and [`ContractValidator`](crate::ContractValidator) refuses for every contract, and
//! it says `advisory: true`: CI never reads it (R-10, P9).

use assay_domain::{Aeu, Decision, Finding, Fingerprint, Tier};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::gates::LocalGate;
use super::review::{LocalRole, Reviewed, Stage};
use crate::prompt_bundle::Role;
use crate::publish::clean;

/// The local report's own schema name: never a contract's.
pub(super) const SCHEMA: &str = "assay.local-check/1";

/// The terminal report's first line.
const BANNER: &str = "assay check --local: advisory, CI never reads this report";

/// How the local review went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewStatus {
    /// The tier has no LLM stage.
    NotRun,
    /// The model returned structured output, validated as `decide` validates it.
    Completed,
    /// The call ran out of budget, turns or time, or its output stayed malformed after the retry.
    Degraded,
    /// No usable call could be made: the CLI did not start, the session was not confined, or
    /// the context pack was not a valid prompt.
    Unavailable,
}

impl ReviewStatus {
    /// Every status.
    const ALL: [Self; 4] = [
        Self::NotRun,
        Self::Completed,
        Self::Degraded,
        Self::Unavailable,
    ];

    /// The status as the report and the terminal write it (`not-run`).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotRun => "not-run",
            Self::Completed => "completed",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }
}

impl Serialize for ReviewStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for ReviewStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Self::ALL
            .into_iter()
            .find(|status| status.code() == code)
            .ok_or_else(|| serde::de::Error::custom("not a review status"))
    }
}

/// A finding as the report keeps it: its fingerprint (03 §3, as validation or the gate's
/// anchoring computed it), then the finding's own fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalFinding {
    /// Its fingerprint.
    pub fp: Fingerprint,
    /// The finding.
    #[serde(flatten)]
    pub finding: Finding,
}

/// The local review as the report shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalReview {
    /// How it went.
    pub status: ReviewStatus,
    /// The role that reviewed; `None` when the tier has no LLM stage.
    pub role: Option<Role>,
    /// Why it did not complete, or what validation refused, in assay's words.
    pub note: Option<String>,
    /// The findings that survived validation, fingerprinted; their free text is the model's.
    pub findings: Vec<LocalFinding>,
    /// What the attempts cost; a call whose cost is unknown is charged its cap.
    pub cost: Aeu,
}

/// [`LocalReview`] as JSON: the role by its code and the cost as AEU text (`0.12`).
#[derive(Serialize)]
struct ShownReview<'r> {
    status: ReviewStatus,
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'r str>,
    findings: &'r [LocalFinding],
    cost: String,
}

impl Serialize for LocalReview {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ShownReview {
            status: self.status,
            role: self.role.map(Role::code),
            note: self.note.as_deref(),
            findings: &self.findings,
            cost: self.cost.to_string(),
        }
        .serialize(serializer)
    }
}

/// The advisory result of `assay check --local`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalReport {
    /// Always [`SCHEMA`].
    pub(super) schema: &'static str,
    /// Always `true`.
    pub(super) advisory: bool,
    pub(super) tree: String,
    /// The base commit and the snapshot commit.
    pub(super) base: String,
    pub(super) head: String,
    pub(super) tier: Tier,
    pub(super) gates: Vec<LocalGate>,
    pub(super) review: LocalReview,
    pub(super) decision: Decision,
}

impl LocalReport {
    /// The tree fingerprint the report is about.
    #[must_use]
    pub fn tree(&self) -> &str {
        &self.tree
    }

    /// The tier `plan` computed for the working tree.
    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.tier
    }

    /// Every scheduled gate, in job then file order.
    #[must_use]
    pub fn gates(&self) -> &[LocalGate] {
        &self.gates
    }

    /// The review.
    #[must_use]
    pub const fn review(&self) -> &LocalReview {
        &self.review
    }

    /// What the rubric decided from the local inputs.
    #[must_use]
    pub const fn decision(&self) -> &Decision {
        &self.decision
    }

    /// The report as pretty JSON ending in a newline, for `.git/assay/<tree>.json`.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when it cannot be encoded.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        Ok(json)
    }

    /// The report for a terminal: one item per line, every gate, model and path text on one
    /// line with its control and format characters replaced, so nothing it quotes can move the
    /// cursor, clear the screen or reorder the text.
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = vec![
            BANNER.to_owned(),
            ["tree ", &clean(&self.tree), ", tier ", self.tier.code()].concat(),
            "gates:".to_owned(),
        ];
        if self.gates.is_empty() {
            lines.push("  none scheduled".to_owned());
        }
        for gate in &self.gates {
            let mut line = [
                "  ",
                &clean(gate.outcome.as_str()),
                " ",
                &clean(&gate.name),
                " (",
                gate.job,
                ")",
            ]
            .concat();
            if let Some(cause) = &gate.cause {
                line.extend([": ", &clean(cause)]);
            }
            lines.push(line);
            lines.extend(
                gate.findings
                    .iter()
                    .map(|shown| finding_line("    ", &shown.finding)),
            );
        }
        let review = &self.review;
        let mut line = ["review: ", review.status.code()].concat();
        if let Some(role) = review.role {
            line.extend([" (", role.code(), ", ", &review.cost.to_string(), " AEU)"]);
        }
        if let Some(note) = &review.note {
            line.extend([": ", &clean(note)]);
        }
        lines.push(line);
        lines.extend(
            review
                .findings
                .iter()
                .map(|shown| finding_line("  ", &shown.finding)),
        );
        lines.push(["decision: ", self.decision.conclusion.code()].concat());
        for reason in &self.decision.reasons {
            lines.push(["  ", reason.code.code(), ": ", &clean(&reason.detail)].concat());
        }
        for degraded in &self.decision.degraded {
            let (role, why) = (clean(&degraded.role), clean(&degraded.reason));
            lines.push(["  degraded ", &role, ": ", &why].concat());
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

/// The review part of the report and the decision, from how the review went.
pub(super) fn review(reviewed: Option<(LocalRole, Reviewed)>) -> LocalReview {
    let Some((role, reviewed)) = reviewed else {
        return LocalReview {
            status: ReviewStatus::NotRun,
            role: None,
            note: None,
            findings: Vec::new(),
            cost: Aeu::ZERO,
        };
    };
    let (status, findings) = match reviewed.stage {
        Stage::Completed(valid) => (
            ReviewStatus::Completed,
            valid
                .into_iter()
                .map(|valid| LocalFinding {
                    fp: valid.fingerprint,
                    finding: valid.finding,
                })
                .collect(),
        ),
        Stage::Degraded(_) => (ReviewStatus::Degraded, Vec::new()),
        Stage::Unavailable => (ReviewStatus::Unavailable, Vec::new()),
    };
    LocalReview {
        status,
        role: Some(role.bundle()),
        note: reviewed.note,
        findings,
        cost: reviewed.cost,
    }
}

/// `<indent><severity> <file>:<line> <title>`.
fn finding_line(indent: &str, finding: &Finding) -> String {
    [
        indent,
        finding.severity.code(),
        " ",
        &clean(&finding.file),
        ":",
        &finding.line.to_string(),
        " ",
        &clean(&finding.title),
    ]
    .concat()
}
