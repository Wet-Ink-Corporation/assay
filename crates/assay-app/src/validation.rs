//! Mechanical validation of reviewer findings, 04 §4 steps 1 to 6 (ADR-08).
//!
//! One pure function, [`validate`], run twice: non-authoritatively in Z3 before the verifier, so
//! a model is never asked to confirm an unsupported claim, and authoritatively in Z4. The only
//! input Z3 lacks is the gate findings, so without them both runs give equal results. Every
//! transition is a [`Transition`] carrying a disposition code (QA-12).

mod anchor;
mod change;
mod evidence;
mod index;
mod screen;
mod steps;
mod symbol;

use std::fmt;

use assay_domain::{
    Dismissal, Disposition, EventPayload, Finding, FindingsDocument, Fingerprint, Severity,
    SourceKind,
};

pub use change::{Change, DiffPaths, FileDiff, Hunk, LineSpan};
pub(crate) use symbol::enclosing_functions;

use crate::contract::{Contract, ContractValidator};
use crate::prompt_bundle::Role;

/// How many injection suspects in one run raise a visible warning (04 §4 step 5).
const INJECTION_WARNING_AT: usize = 2;

/// The most findings of one document validated; each costs a search of its file, so the rest
/// are dropped as `cap-exceeded` before step 2 (caps enforced in validation, wi-641e1a).
const MAX_FINDINGS_PER_DOCUMENT: usize = 256;

/// The base `suppress` rules (`.assay/suppress.toml`, 03 §1), as the caller parsed them.
///
/// A trait rather than data because how a rule's ids and path globs combine is the parser's to
/// define (03 §1 lists only "rule ids and path globs"). Any implementation must be a pure
/// function of its arguments, or Z3 and Z4 can disagree.
pub trait SuppressRules {
    /// Whether a rule suppresses `finding`, whose file is `file_at_head` after rename
    /// resolution.
    fn suppresses(&self, finding: &Finding, file_at_head: &str) -> bool;
}

/// What a run validates its reviewer documents against.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    /// The repository under review, `owner/name`: links into it pass the injection screen.
    pub repository: &'a str,
    /// The change under review.
    pub change: &'a Change,
    /// The base `suppress` rules.
    pub suppress: &'a dyn SuppressRules,
    /// The owner-valid dismissals from the context pack's command records (04 §6).
    pub dismissals: &'a [Dismissal<'a>],
    /// Gate findings, in Z4 only; Z3 runs before the gates finish and passes none (04 §1).
    pub gate_findings: &'a [Finding],
}

impl fmt::Debug for Inputs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inputs")
            .field("repository", &self.repository)
            .field("change", &self.change)
            .field("dismissals", &self.dismissals)
            .field("gate_findings", &self.gate_findings)
            .finish_non_exhaustive()
    }
}

/// Which input a transition or a finding refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FindingRef {
    /// A whole document, rejected before its findings could be read.
    Document {
        /// Its position in the documents passed to [`validate`].
        document: usize,
    },
    /// One finding of a document.
    Finding {
        /// Its document's position in the documents passed to [`validate`].
        document: usize,
        /// Its position in that document's `findings`.
        index: usize,
    },
}

/// Where a kept finding is published (04 §4 step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placement {
    /// As an inline comment: its line is in a diff hunk.
    Inline,
    /// In the summary only; it still counts for the rubric.
    Summary,
}

/// One disposition applied to one input (QA-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    /// The input it applies to.
    pub at: FindingRef,
    /// What happened.
    pub code: Disposition,
    /// The severity before, when the transition changes it.
    pub from: Option<Severity>,
    /// The severity after, when the transition changes it.
    pub to: Option<Severity>,
}

impl Transition {
    /// A transition that leaves the severity alone.
    const fn of(at: FindingRef, code: Disposition) -> Self {
        Self {
            at,
            code,
            from: None,
            to: None,
        }
    }

    /// The `FindingDisposed` event recording this transition (03 §6).
    #[must_use]
    pub const fn event(&self) -> EventPayload {
        EventPayload::FindingDisposed {
            code: self.code,
            from: self.from,
            to: self.to,
        }
    }
}

/// A finding that survived validation, with what validation established about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidFinding {
    /// Where it came from.
    pub at: FindingRef,
    /// The finding: `line` and `end_line` relocated to its evidence, `side` made explicit (base
    /// for a deleted file), `severity` capped without a trigger, `pre_existing` kept only where
    /// honoured, and an interaction's `caller.line` relocated too.
    pub finding: Finding,
    /// Its file's path at head, rename-resolved; the base path of a deleted file.
    pub file_at_head: String,
    /// The function its line belongs to, empty at file scope.
    pub enclosing_symbol: String,
    /// How many earlier matches of its evidence the same symbol holds.
    pub occurrence_index: u32,
    /// Its fingerprint (03 §3); authoritative only from Z4.
    pub fingerprint: Fingerprint,
    /// Inline or summary.
    pub placement: Placement,
    /// The highest severity of an owner dismissal it resembles at a lower severity, to publish
    /// as "previously dismissed similar" (03 §3).
    pub previously_dismissed: Option<Severity>,
}

/// The outcome of validating one run's reviewer documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    /// The findings that survived, in input order.
    pub findings: Vec<ValidFinding>,
    /// Every transition, in the order applied; a dropped finding's last one drops it.
    pub transitions: Vec<Transition>,
}

impl Validated {
    /// How many findings the injection screen dropped.
    #[must_use]
    pub fn injection_suspects(&self) -> usize {
        self.transitions
            .iter()
            .filter(|transition| transition.code == Disposition::InjectionSuspect)
            .count()
    }

    /// Whether the run shows a visible injection warning: two or more suspects (04 §4 step 5).
    #[must_use]
    pub fn raises_injection_warning(&self) -> bool {
        self.injection_suspects() >= INJECTION_WARNING_AT
    }

    /// One `FindingDisposed` event per transition, in order.
    pub fn events(&self) -> impl Iterator<Item = EventPayload> + '_ {
        self.transitions.iter().map(Transition::event)
    }
}

/// Validates reviewer findings `documents` (raw `assay.findings/1` bytes) against `inputs`.
///
/// Applies 04 §4 steps 1 to 6 in order: schema, evidence location, placement, severity caps,
/// injection screen and suppression. Pure and total: every input ends kept or with a
/// disposition. A document that fails its schema, is not a reviewer's or names a role outside
/// the roster is one `schema` transition. Each file is indexed once per call, however many
/// findings cite it. No transition quotes model text. A kept finding's `evidence` and
/// `caller.evidence` are replaced by the tree's lines; its other free-text fields are still
/// model text, which the injection screen checks heuristically, refusing what it cannot be
/// sure GitHub renders harmlessly.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use assay_app::{Change, ContractValidator, Inputs, Placement, SuppressRules, validate};
/// use assay_domain::Finding;
///
/// struct NoRules;
///
/// impl SuppressRules for NoRules {
///     fn suppresses(&self, _finding: &Finding, _file_at_head: &str) -> bool {
///         false
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let lib = "fn f() {\n    record_event(id);\n}\n";
/// let change = Change {
///     head_files: BTreeMap::from([("src/lib.rs".to_owned(), lib.to_owned())]),
///     ..Change::default()
/// };
/// let inputs = Inputs {
///     repository: "owner/repo",
///     change: &change,
///     suppress: &NoRules,
///     dismissals: &[],
///     gate_findings: &[],
/// };
/// let document = br#"{"schema": "assay.findings/1",
///     "source": {"kind": "reviewer", "name": "claude", "version": "v", "role": "combined",
///                "run_key": "r"},
///     "findings": [{"class": "logic", "kind": "defect", "severity": "minor", "file": "src/lib.rs",
///                   "line": 9, "evidence": "record_event(id);", "title": "t", "body": "b"}]}"#;
///
/// let validated = validate(&ContractValidator::new()?, &inputs, &[document]);
///
/// // Relocated from line 9 to its evidence on line 2, which no hunk covers.
/// let kept = validated.findings.first().ok_or("the finding is kept")?;
/// assert_eq!((kept.finding.line.get(), kept.placement), (2, Placement::Summary));
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn validate(
    validator: &ContractValidator,
    inputs: &Inputs<'_>,
    documents: &[&[u8]],
) -> Validated {
    let mut run = steps::Run::new(inputs);
    for (document, bytes) in documents.iter().enumerate() {
        match read(validator, bytes) {
            Ok((ceiling, findings)) => {
                for (index, finding) in findings.into_iter().enumerate() {
                    let at = FindingRef::Finding { document, index };
                    if index < MAX_FINDINGS_PER_DOCUMENT {
                        run.check(at, finding, ceiling);
                    } else {
                        run.reject(at, Disposition::CapExceeded);
                    }
                }
            }
            Err(code) => run.reject(FindingRef::Document { document }, code),
        }
    }
    run.finish()
}

/// Step 1: the findings of a schema-valid reviewer document, with the severity ceiling of the
/// role that wrote them, or the `schema` disposition. A role outside the roster is refused:
/// every reviewer role has a ceiling, and an unknown one cannot be held to it.
fn read(
    validator: &ContractValidator,
    bytes: &[u8],
) -> Result<(Severity, Vec<Finding>), Disposition> {
    let value = validator
        .read(Contract::Findings, bytes)
        .map_err(|error| error.disposition())?;
    // The schema accepted it, so a shape error here is a drift between schema and type; it is
    // still a schema rejection, never a pass.
    let document: FindingsDocument = match serde_json::from_value(value) {
        Ok(document) => document,
        Err(_) => return Err(Disposition::Schema),
    };
    if document.source.kind != SourceKind::Reviewer {
        return Err(Disposition::Schema);
    }
    let role = document
        .source
        .role
        .as_deref()
        .and_then(|code| Role::ALL.into_iter().find(|role| role.code() == code))
        .ok_or(Disposition::Schema)?;
    Ok((role.severity_ceiling(), document.findings))
}
