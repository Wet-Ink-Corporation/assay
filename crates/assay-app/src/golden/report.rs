//! The gate's findings: one per problem, text clipped to the `assay.findings/1` caps so the
//! document always validates, and no PR-controlled text in a title or body.

use std::num::NonZeroU64;

use assay_domain::{Class, Finding, FindingKind, Severity, Side};

use crate::finding_text::{clip_chars, plain_name};

/// Most characters of `evidence` (03 §3).
const EVIDENCE_CHARS: usize = 400;
/// Most characters of `title`.
const TITLE_CHARS: usize = 100;

/// What is wrong at one path of the corpus or the rubric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Problem {
    /// The replayed decision differs from the recorded one, first at this 1-based line.
    Mismatch {
        /// The line of the recorded file.
        line: usize,
    },
    /// The replayed decision could not be written as JSON, so it matches nothing.
    Unwritable,
    /// `inputs.json` is not `DecisionInputs`; serde's 1-based line (0 when it has none).
    InputsInvalid {
        /// The line serde points at.
        line: usize,
    },
    /// The rubric does not parse, at this 1-based line.
    RubricInvalid {
        /// The line the parser points at.
        line: usize,
    },
    /// A case lacks this file.
    CaseIncomplete,
    /// An entry the corpus allowlist does not name.
    UnexpectedEntry,
    /// The corpus holds no case.
    CorpusEmpty,
}

impl Problem {
    /// The finding for this problem at `file`, quoting the line it points at in `quoted` when
    /// there is one to quote, else the path.
    pub(super) fn finding(self, file: &str, quoted: Option<&[u8]>) -> Finding {
        let (rule, class, title, body) = self.text();
        let line = match self {
            Self::Mismatch { line }
            | Self::InputsInvalid { line }
            | Self::RubricInvalid { line } => line,
            Self::Unwritable | Self::CaseIncomplete | Self::UnexpectedEntry | Self::CorpusEmpty => {
                1
            }
        };
        let evidence = quoted
            .and_then(|bytes| line_text(bytes, line))
            .unwrap_or_else(|| clip_chars(&plain_name(file), EVIDENCE_CHARS));
        Finding {
            class,
            rule: Some(rule.to_owned()),
            kind: FindingKind::Defect,
            severity: Severity::Blocker,
            introduced_by: None,
            pre_existing: false,
            file: file.to_owned(),
            line: one_based(line),
            end_line: None,
            side: Some(Side::Right),
            evidence,
            expected_behavior: None,
            trigger: None,
            caller: None,
            title: clip_chars(&format!("{title}: `{}`", plain_name(file)), TITLE_CHARS),
            body: body.to_owned(),
            fix: None,
            confidence: None,
            group_hint: None,
        }
    }

    /// The rule id, class, title stem and body of this problem's finding.
    const fn text(self) -> (&'static str, Class, &'static str, &'static str) {
        match self {
            Self::Mismatch { .. } => (
                "golden-mismatch",
                Class::Logic,
                "Golden decision changed",
                "Replaying `decide` over this case with the PR's rubric gives other bytes than \
                 the recorded snapshot, from this line on: a verdict change. If it is intended, \
                 update the snapshot in this PR; `corpus/golden/` is a trust path, so the \
                 update needs owner approval.",
            ),
            Self::Unwritable => (
                "golden-mismatch",
                Class::Logic,
                "Golden decision not writable",
                "The decision `decide` returns for this case could not be written as JSON, so \
                 it cannot match the recorded snapshot.",
            ),
            Self::InputsInvalid { .. } => (
                "golden-inputs-invalid",
                Class::Tests,
                "Golden inputs unreadable",
                "This case's `inputs.json` is not a `DecisionInputs` document, so the case \
                 cannot be replayed.",
            ),
            Self::RubricInvalid { .. } => (
                "golden-rubric-invalid",
                Class::Logic,
                "Rubric unreadable",
                "The rubric does not parse, so no golden case can be replayed.",
            ),
            Self::CaseIncomplete => (
                "golden-case-incomplete",
                Class::Tests,
                "Golden case incomplete",
                "Every golden case holds both `inputs.json` and `decision.json`; this one lacks \
                 the named file, so it cannot be replayed.",
            ),
            Self::UnexpectedEntry => (
                "golden-unexpected-entry",
                Class::Tests,
                "Unexpected entry in the golden corpus",
                "The golden corpus holds only `README.md` and case directories, each with a \
                 regular `inputs.json` and `decision.json`, each once. Anything else is refused \
                 rather than skipped.",
            ),
            Self::CorpusEmpty => (
                "golden-corpus-empty",
                Class::Tests,
                "Golden corpus empty",
                "The golden corpus holds no case, so the replay would check nothing.",
            ),
        }
    }
}

/// The 1-based line of `bytes` that holds the byte at `offset`; an offset at or past the end
/// counts as the last line.
pub(super) fn line_at(bytes: &[u8], offset: usize) -> usize {
    let before = bytes.get(..offset).unwrap_or(bytes);
    let newlines = before.iter().filter(|byte| **byte == b'\n').count();
    let last = lines(bytes).count().max(1);
    newlines.saturating_add(1).min(last)
}

/// The lines of `bytes`, without a final empty one after a trailing newline.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .split(|byte| *byte == b'\n')
}

/// Line `line` (1-based) of `bytes`, trimmed and clipped, or `None` when it does not exist or
/// holds nothing to quote.
fn line_text(bytes: &[u8], line: usize) -> Option<String> {
    let text = lines(bytes).nth(line.checked_sub(1)?)?;
    let text = String::from_utf8_lossy(text);
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| clip_chars(trimmed, EVIDENCE_CHARS))
}

/// `line` as a finding's line; line 1 for 0 (serde's "no line").
fn one_based(line: usize) -> NonZeroU64 {
    u64::try_from(line)
        .ok()
        .and_then(NonZeroU64::new)
        .unwrap_or(NonZeroU64::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &[u8] = b"{\n  \"a\": 1,\n\n  \"b\": 2\n}\n";

    #[test]
    fn a_path_quoted_as_evidence_is_reduced_like_the_title() {
        let finding = Problem::UnexpectedEntry.finding("corpus/golden/x`\n[l](https://e)", None);

        assert_eq!(
            finding.evidence, "corpus/golden/x???l??https?e?",
            "no PR-controlled backtick, newline, markup or link reaches the evidence"
        );
    }

    #[test]
    fn line_at_counts_the_newlines_before_the_offset() {
        assert_eq!(
            [0, 2, 12, 14].map(|offset| line_at(TEXT, offset)),
            [1, 2, 3, 4],
            "offsets in lines 1, 2, the blank line 3 and line 4"
        );
    }

    #[test]
    fn an_offset_past_the_end_is_the_last_line() {
        assert_eq!(line_at(TEXT, TEXT.len()), 5, "the closing brace");
    }

    #[test]
    fn a_blank_line_quotes_nothing() {
        assert_eq!(
            (line_text(TEXT, 2), line_text(TEXT, 3), line_text(TEXT, 6)),
            (Some("\"a\": 1,".to_owned()), None, None),
            "a line with text, a blank line and a line past the end"
        );
    }

    #[test]
    fn a_path_cannot_break_out_of_a_code_span() {
        assert_eq!(
            plain_name("corpus/golden/x`\n[link](https://e)"),
            "corpus/golden/x???link??https?e?",
            "backticks, newlines, markup and `://` are replaced"
        );
    }
}
