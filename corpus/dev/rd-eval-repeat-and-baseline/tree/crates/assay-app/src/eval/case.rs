//! One eval case: its `case.json` (format `assay.eval-case/1`, read strictly) and its diff.

use std::fmt;
use std::num::NonZeroU64;

use assay_domain::Class;
use serde::Deserialize;
use thiserror::Error;

use crate::contract::parse_json_strict;

/// The `schema` every `case.json` carries.
pub const CASE_SCHEMA: &str = "assay.eval-case/1";

/// The most labels (fixing hunks) one case may hold.
pub const MAX_LABELS: usize = 64;

/// The longest authoring model id, in bytes.
pub const MAX_AUTHORING_MODEL_BYTES: usize = 64;

/// The longest labelled file path, in bytes.
pub const MAX_LABEL_PATH_BYTES: usize = 1024;

/// The authoring model's id for a human-written change.
const HUMAN: &str = "human";

/// Who wrote the change under review (04 §8), so self-review recall is reported apart from
/// human-authored recall.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuthoringModel {
    /// A person (`"human"`).
    Human,
    /// A model, by id: 1 to 64 bytes of `a-z`, `0-9`, `.`, `_` and `-` (`claude-opus-5-5`).
    Model(String),
}

impl AuthoringModel {
    /// Reads `"human"` or a model id; `None` for anything else.
    fn parse(text: String) -> Option<Self> {
        if text == HUMAN {
            return Some(Self::Human);
        }
        let allowed = |byte: &u8| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        };
        let shaped = !text.is_empty()
            && text.len() <= MAX_AUTHORING_MODEL_BYTES
            && text.as_bytes().iter().all(allowed);
        shaped.then_some(Self::Model(text))
    }
}

impl fmt::Display for AuthoringModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Human => f.write_str(HUMAN),
            Self::Model(id) => f.write_str(id),
        }
    }
}

/// Where a case came from (04 §8's sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseSource {
    /// A real PR whose merged code later needed a fix; the labels are the fix's hunks.
    RealDefect,
    /// A mutation-seeded defect (smoke only).
    Mutation,
    /// A PR with nothing to find, for noise. Holds no label.
    TrueNegative,
    /// Hostile text the pipeline must survive; labelled or not.
    Injection,
    /// A rebase or force-push fixture.
    Rebase,
    /// A cross-file "consumer not updated" fixture.
    CrossFile,
}

/// One fixing hunk: a file, its lines at the reviewed head (inclusive) and the defect's class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    file: String,
    first: NonZeroU64,
    last: NonZeroU64,
    class: Class,
}

impl Label {
    /// A label; refused when `last` is before `first` or the path is not a plain relative
    /// path (empty, absolute, over [`MAX_LABEL_PATH_BYTES`], holding NUL, or with an empty,
    /// `.` or `..` segment).
    pub(crate) fn new(
        file: impl Into<String>,
        first: NonZeroU64,
        last: NonZeroU64,
        class: Class,
    ) -> Result<Self, LabelProblem> {
        let file = file.into();
        if first > last {
            return Err(LabelProblem::Span);
        }
        let plain = file.len() <= MAX_LABEL_PATH_BYTES
            && !file.contains('\0')
            && file
                .split('/')
                .all(|segment| !matches!(segment, "" | "." | ".."));
        if !plain {
            return Err(LabelProblem::Path);
        }
        Ok(Self {
            file,
            first,
            last,
            class,
        })
    }

    /// The file, relative to the repository root.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The hunk's first line.
    #[must_use]
    pub const fn first(&self) -> NonZeroU64 {
        self.first
    }

    /// The hunk's last line.
    #[must_use]
    pub const fn last(&self) -> NonZeroU64 {
        self.last
    }

    /// The defect's class.
    #[must_use]
    pub const fn class(&self) -> Class {
        self.class
    }
}

/// Why a label was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LabelProblem {
    /// It ends before it starts.
    Span,
    /// Its path is not a plain relative path.
    Path,
}

/// Why a `case.json` was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CaseProblem {
    /// Not one JSON object of exactly the case fields (an unknown or repeated key, a wrong
    /// type, an unknown source or class), at this 1-based line.
    #[error("case.json is not an {CASE_SCHEMA} document (line {line})")]
    Syntax {
        /// The line the parser stopped at.
        line: usize,
    },
    /// The `schema` is not [`CASE_SCHEMA`].
    #[error("case.json is not {CASE_SCHEMA}")]
    Schema,
    /// The `authoring_model` is neither `human` nor a model id.
    #[error("the authoring model is neither \"human\" nor a model id")]
    AuthoringModel,
    /// A `true-negative` case holds a label.
    #[error("a true-negative case holds a label")]
    LabelsOnTrueNegative,
    /// A defect case (any source but `true-negative` and `injection`) holds no label.
    #[error("a defect case holds no label")]
    NoLabels,
    /// More than [`MAX_LABELS`] labels.
    #[error("more than {MAX_LABELS} labels")]
    TooManyLabels,
    /// The label at `index` ends before it starts.
    #[error("label {index} ends before it starts")]
    LabelSpan {
        /// The label's 0-based position.
        index: usize,
    },
    /// The label at `index` names no plain relative path.
    #[error("label {index} names no plain relative path")]
    LabelPath {
        /// The label's 0-based position.
        index: usize,
    },
}

/// A `case.json`, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseDocument {
    schema: String,
    authoring_model: String,
    source: CaseSource,
    labels: Vec<LabelDocument>,
}

/// One label, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelDocument {
    file: String,
    start_line: NonZeroU64,
    end_line: NonZeroU64,
    class: Class,
}

/// What a `case.json` holds once read.
#[derive(Debug)]
pub(crate) struct CaseFile {
    pub(crate) authoring_model: AuthoringModel,
    pub(crate) source: CaseSource,
    pub(crate) labels: Vec<Label>,
}

/// Reads a `case.json` strictly. The caller bounds its size.
pub(crate) fn read_case(json: &[u8]) -> Result<CaseFile, CaseProblem> {
    let syntax = |error: serde_json::Error| CaseProblem::Syntax { line: error.line() };
    let value = parse_json_strict(json).map_err(syntax)?;
    let document = CaseDocument::deserialize(value).map_err(syntax)?;
    if document.schema != CASE_SCHEMA {
        return Err(CaseProblem::Schema);
    }
    let authoring_model =
        AuthoringModel::parse(document.authoring_model).ok_or(CaseProblem::AuthoringModel)?;
    if document.labels.len() > MAX_LABELS {
        return Err(CaseProblem::TooManyLabels);
    }
    match document.source {
        CaseSource::TrueNegative if !document.labels.is_empty() => {
            return Err(CaseProblem::LabelsOnTrueNegative);
        }
        CaseSource::TrueNegative | CaseSource::Injection => {}
        _ if document.labels.is_empty() => return Err(CaseProblem::NoLabels),
        _ => {}
    }
    let labels = document
        .labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| {
            Label::new(label.file, label.start_line, label.end_line, label.class).map_err(
                |problem| match problem {
                    LabelProblem::Span => CaseProblem::LabelSpan { index },
                    LabelProblem::Path => CaseProblem::LabelPath { index },
                },
            )
        })
        .collect::<Result<_, _>>()?;
    Ok(CaseFile {
        authoring_model,
        source: document.source,
        labels,
    })
}
