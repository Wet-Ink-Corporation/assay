//! The intake of findings carried from earlier runs (`docs/architecture/04-pipeline.md` §7).
//!
//! Provenance: a carried finding is one this PR's earlier runs published and that is still
//! open. `admit` (Z4) builds the set from App-authored records only, the earlier runs' persisted
//! bundles by ledger pointer, never from PR comment text (02 §2). Its text was still written by
//! a model, so it is untrusted: every field is checked here, the set is capped, and anything
//! outside the shape is refused, never repaired (wi-b5affa).

use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU64;

use assay_domain::{Class, Fingerprint, Severity, VerifierVerdict};
use thiserror::Error;

use crate::validation::{is_injection_suspect, is_invisible, is_searchable_evidence};

/// The most findings carried into one run. The set is refused whole past it, before any of it
/// is read.
pub const MAX_CARRIED: usize = 256;

/// The longest carried path, in bytes.
pub const MAX_CARRIED_PATH_BYTES: usize = 4096;

/// The longest carried title, in characters (`findings-1`'s cap).
const MAX_TITLE_CHARS: usize = 100;

/// The longest carried trigger, in characters (`findings-1`'s cap).
const MAX_TRIGGER_CHARS: usize = 300;

/// The longest carried evidence, in characters (`findings-1`'s cap).
const MAX_EVIDENCE_CHARS: usize = 400;

/// A carried finding as the caller read it from the earlier run's record, not yet checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedParts {
    /// Its fingerprint, as Z4 computed it when it was published.
    pub fp: Fingerprint,
    /// Its file's path as last published, relative to the repository root.
    pub file: String,
    /// Its line as last published.
    pub line: NonZeroU64,
    /// The last line of its range as last published, when it spans several.
    pub end_line: Option<NonZeroU64>,
    /// Its class.
    pub class: Class,
    /// Its severity as last published (after that run's verification).
    pub severity: Severity,
    /// The verifier's verdict on it when it was last re-verified, if it ever was.
    pub verdict: Option<VerifierVerdict>,
    /// Its title (model text).
    pub title: String,
    /// How the defect is reached (model text), if it said.
    pub trigger: Option<String>,
    /// The tree's lines its evidence matched when it was published, searched for again at head.
    pub evidence: String,
}

/// Whether a carried finding's evidence was looked for at head yet, and where it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CarriedAnchor {
    /// Not looked for yet: the finding is where it was last published.
    Unchecked,
    /// Its evidence is still at head, and the finding has moved to it.
    AtHead,
    /// Its evidence is not at head: the finding needs re-verification, which is not resolution
    /// (04 §7).
    Gone,
}

/// A finding open from an earlier run of the PR, to be judged again at head (04 §7).
///
/// Built only by [`CarriedFinding::read`]. It goes to the verifier whatever its severity, ahead
/// of this run's candidates, and shares the tier's calls with them. Only its `file`, `line`,
/// `class`, `title` and `trigger` reach a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedFinding {
    fp: Fingerprint,
    file: String,
    line: NonZeroU64,
    end_line: Option<NonZeroU64>,
    class: Class,
    severity: Severity,
    verdict: Option<VerifierVerdict>,
    title: String,
    trigger: Option<String>,
    evidence: String,
    anchor: CarriedAnchor,
}

/// The field of a carried finding a refusal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CarriedField {
    /// `file`.
    File,
    /// `end_line`.
    EndLine,
    /// `title`.
    Title,
    /// `trigger`.
    Trigger,
    /// `evidence`.
    Evidence,
}

/// What is wrong with a refused field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CarriedProblem {
    /// It is empty or only whitespace.
    Empty,
    /// It is longer than its cap.
    TooLong,
    /// It holds a control character (line breaks included) or an invisible one (zero-width,
    /// direction, tag or another format character).
    Control,
    /// A path that starts with `/`.
    Absolute,
    /// A path with an empty, `.` or `..` segment.
    Segment,
    /// The injection screen of validation step 5 flags it.
    Injection,
    /// Evidence with too little content to be searched for at head.
    NoContent,
    /// A range that ends before its first line.
    BeforeLine,
}

/// Why one carried finding was refused. Shows neither the field's text nor anything from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("its {field} is refused: {problem}")]
pub struct CarriedRefusal {
    /// The field refused.
    pub field: CarriedField,
    /// Why.
    pub problem: CarriedProblem,
}

/// Why a set of carried findings was refused whole. Shows no carried text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum CarryRefused {
    /// More than [`MAX_CARRIED`] findings; none was read.
    #[error("{count} carried findings, more than {MAX_CARRIED}")]
    TooMany {
        /// How many were given.
        count: usize,
    },
    /// The finding at `index` was refused.
    #[error("carried finding {index} is refused")]
    Finding {
        /// Its position in the set.
        index: usize,
        /// Why.
        #[source]
        refusal: CarriedRefusal,
    },
    /// The finding at `index` repeats an earlier one's fingerprint.
    #[error("carried finding {index} repeats an earlier fingerprint")]
    Repeated {
        /// Its position in the set.
        index: usize,
    },
}

/// Reads a set of carried findings for a change to `repository` (`owner/name`), in order.
///
/// Fail closed: one refused finding refuses the set, so the run re-judges nothing it cannot
/// trust; a carried finding that is not re-judged is never resolved, and stays as last verified.
///
/// # Errors
/// [`CarryRefused::TooMany`] past [`MAX_CARRIED`], before any finding is read;
/// [`CarryRefused::Finding`] for the first finding [`CarriedFinding::read`] refuses; and
/// [`CarryRefused::Repeated`] for the first fingerprint given twice.
pub fn read_carried(
    parts: Vec<CarriedParts>,
    repository: &str,
) -> Result<Vec<CarriedFinding>, CarryRefused> {
    let count = parts.len();
    if count > MAX_CARRIED {
        return Err(CarryRefused::TooMany { count });
    }
    let mut seen = BTreeSet::new();
    let mut read = Vec::with_capacity(count);
    for (index, part) in parts.into_iter().enumerate() {
        if !seen.insert(part.fp) {
            return Err(CarryRefused::Repeated { index });
        }
        let finding = CarriedFinding::read(part, repository)
            .map_err(|refusal| CarryRefused::Finding { index, refusal })?;
        read.push(finding);
    }
    Ok(read)
}

impl CarriedFinding {
    /// Checks `parts`, for a change to `repository` (`owner/name`), with its anchor
    /// [`CarriedAnchor::Unchecked`]. The refused text is untrusted and never retried, so it is
    /// dropped rather than handed back.
    ///
    /// # Errors
    /// [`CarriedRefusal`] naming the first refused field, in the order `file`, `end_line`,
    /// `title`, `trigger`, `evidence`: a path that is empty, longer than
    /// [`MAX_CARRIED_PATH_BYTES`], holds a control or invisible character, is absolute or has an
    /// empty, `.` or `..` segment; a range that ends before its line; a title (or a trigger
    /// given) that is blank, longer than its `findings-1` cap (100 and 300 characters), holds a
    /// control or invisible character or is flagged by the injection screen; evidence longer
    /// than 400 characters or with too little content to search for.
    pub fn read(parts: CarriedParts, repository: &str) -> Result<Self, CarriedRefusal> {
        let CarriedParts {
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
        } = parts;
        check_path(&file).map_err(|problem| refused(CarriedField::File, problem))?;
        if end_line.is_some_and(|end| end < line) {
            return Err(refused(CarriedField::EndLine, CarriedProblem::BeforeLine));
        }
        check_prose(&title, MAX_TITLE_CHARS, repository)
            .map_err(|problem| refused(CarriedField::Title, problem))?;
        if let Some(trigger) = &trigger {
            check_prose(trigger, MAX_TRIGGER_CHARS, repository)
                .map_err(|problem| refused(CarriedField::Trigger, problem))?;
        }
        check_evidence(&evidence).map_err(|problem| refused(CarriedField::Evidence, problem))?;
        Ok(Self {
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
            anchor: CarriedAnchor::Unchecked,
        })
    }

    /// Its fingerprint, as published.
    #[must_use]
    pub const fn fp(&self) -> Fingerprint {
        self.fp
    }

    /// Its file's path: at head once its anchor is [`CarriedAnchor::AtHead`], otherwise as last
    /// published.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// Its line: at head once its anchor is [`CarriedAnchor::AtHead`], otherwise as last
    /// published.
    #[must_use]
    pub const fn line(&self) -> NonZeroU64 {
        self.line
    }

    /// The last line of its range, when it spans several: moved with [`Self::line`].
    #[must_use]
    pub const fn end_line(&self) -> Option<NonZeroU64> {
        self.end_line
    }

    /// Its class.
    #[must_use]
    pub const fn class(&self) -> Class {
        self.class
    }

    /// Its severity as last published.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    /// The verdict it was last re-verified with, if it ever was.
    #[must_use]
    pub const fn verdict(&self) -> Option<VerifierVerdict> {
        self.verdict
    }

    /// Its title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// How the defect is reached, if it said.
    #[must_use]
    pub fn trigger(&self) -> Option<&str> {
        self.trigger.as_deref()
    }

    /// The tree's lines its evidence matched when it was published.
    #[must_use]
    pub fn evidence(&self) -> &str {
        &self.evidence
    }

    /// Whether its evidence was looked for at head, and found.
    #[must_use]
    pub const fn anchor(&self) -> CarriedAnchor {
        self.anchor
    }

    /// The finding moved to `line` of `file` at head, where its evidence still is; its range
    /// keeps its length (and is dropped only if it would pass `u64::MAX`).
    pub(super) fn at_head(self, file: String, line: NonZeroU64) -> Self {
        let end_line = self.end_line.and_then(|end| {
            end.get()
                .checked_sub(self.line.get())
                .and_then(|span| line.checked_add(span))
        });
        Self {
            file,
            line,
            end_line,
            anchor: CarriedAnchor::AtHead,
            ..self
        }
    }

    /// The finding as this run judged it: its severity and verdict after verification.
    pub(super) fn judged(self, severity: Severity, verdict: Option<VerifierVerdict>) -> Self {
        Self {
            severity,
            verdict,
            ..self
        }
    }

    /// The finding where it was last published, its evidence gone at head.
    pub(super) fn gone(self) -> Self {
        Self {
            anchor: CarriedAnchor::Gone,
            ..self
        }
    }
}

const fn refused(field: CarriedField, problem: CarriedProblem) -> CarriedRefusal {
    CarriedRefusal { field, problem }
}

/// A control character, or one the injection screen reads as invisible.
fn is_hidden(c: char) -> bool {
    c.is_control() || is_invisible(c)
}

/// A repository-relative path of plain segments, within its cap.
fn check_path(path: &str) -> Result<(), CarriedProblem> {
    if path.is_empty() {
        Err(CarriedProblem::Empty)
    } else if path.len() > MAX_CARRIED_PATH_BYTES {
        Err(CarriedProblem::TooLong)
    } else if path.chars().any(is_hidden) {
        Err(CarriedProblem::Control)
    } else if path.starts_with('/') {
        Err(CarriedProblem::Absolute)
    } else if path
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".."))
    {
        Err(CarriedProblem::Segment)
    } else {
        Ok(())
    }
}

/// One line of model prose within `max_chars`, clear of the injection screen. The length is
/// checked first, so the screen reads at most `max_chars` characters.
fn check_prose(text: &str, max_chars: usize, repository: &str) -> Result<(), CarriedProblem> {
    if text.trim().is_empty() {
        Err(CarriedProblem::Empty)
    } else if text.chars().count() > max_chars {
        Err(CarriedProblem::TooLong)
    } else if text.chars().any(is_hidden) {
        Err(CarriedProblem::Control)
    } else if is_injection_suspect(text, repository) {
        Err(CarriedProblem::Injection)
    } else {
        Ok(())
    }
}

/// Evidence within its cap with enough content to search for. It is the tree's own text and
/// never reaches a model, so it is not screened.
fn check_evidence(evidence: &str) -> Result<(), CarriedProblem> {
    if evidence.chars().count() > MAX_EVIDENCE_CHARS {
        Err(CarriedProblem::TooLong)
    } else if !is_searchable_evidence(evidence) {
        Err(CarriedProblem::NoContent)
    } else {
        Ok(())
    }
}

impl fmt::Display for CarriedField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::File => "file",
            Self::EndLine => "end_line",
            Self::Title => "title",
            Self::Trigger => "trigger",
            Self::Evidence => "evidence",
        })
    }
}

impl fmt::Display for CarriedProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "it is empty",
            Self::TooLong => "it is longer than its cap",
            Self::Control => "it holds a control or invisible character",
            Self::Absolute => "it is absolute",
            Self::Segment => "it has an empty, `.` or `..` segment",
            Self::Injection => "the injection screen flags it",
            Self::NoContent => "it has too little content to search for",
            Self::BeforeLine => "it ends before its first line",
        })
    }
}
