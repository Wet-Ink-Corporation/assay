//! Building the adapter's findings: where each sits, and text clipped to the
//! `assay.findings/1` caps so the document always validates.

use std::num::NonZeroU64;

use assay_domain::{Class, Finding, FindingKind, Severity, Side};

/// Most characters of `evidence` (03 §3).
const EVIDENCE_CHARS: usize = 400;
/// Most characters of `title`.
const TITLE_CHARS: usize = 100;
/// Most characters of `body`.
const BODY_CHARS: usize = 1200;

/// Where a finding sits and the span it quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Spot {
    pub(super) file: String,
    pub(super) line: NonZeroU64,
    pub(super) side: Side,
    pub(super) evidence: String,
}

impl Spot {
    /// The first line of `text` reached by finding each of `anchors` in turn (each on or after
    /// the line of the one before), quoted trimmed; `None` when an anchor is not found.
    pub(super) fn locate(file: &str, side: Side, text: &str, anchors: &[&str]) -> Option<Self> {
        let lines: Vec<&str> = text.lines().collect();
        let mut at = 0_usize;
        for anchor in anchors {
            let offset = lines
                .get(at..)?
                .iter()
                .position(|line| line.contains(anchor))?;
            at = at.checked_add(offset)?;
        }
        let evidence = line_evidence(lines.get(at)?)?;
        let line = NonZeroU64::new(u64::try_from(at).ok()?.checked_add(1)?)?;
        Some(Self {
            file: file.to_owned(),
            line,
            side,
            evidence,
        })
    }

    /// `line` of `file` at head, quoting `quote` trimmed, or `fallback` when there is nothing
    /// to quote.
    pub(super) fn at(file: String, line: NonZeroU64, quote: Option<&str>, fallback: &str) -> Self {
        Self {
            file,
            line,
            side: Side::Right,
            evidence: quote
                .and_then(line_evidence)
                .unwrap_or_else(|| clip(fallback, EVIDENCE_CHARS)),
        }
    }

    /// Line 1 of `file`, quoting `evidence` that the adapter wrote (03 §2.1: gate evidence is
    /// synthesised by the adapter).
    pub(super) fn synthesised(file: &str, evidence: &str) -> Self {
        Self {
            file: file.to_owned(),
            line: NonZeroU64::MIN,
            side: Side::Right,
            evidence: clip(evidence, EVIDENCE_CHARS),
        }
    }
}

/// `line` trimmed and clipped, or `None` when nothing is left to quote.
fn line_evidence(line: &str) -> Option<String> {
    let trimmed = line.trim();
    (!trimmed.is_empty()).then(|| clip(trimmed, EVIDENCE_CHARS))
}

/// A rule of the temper adapter: its id in findings, and how its findings are classed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rule {
    pub(super) id: &'static str,
    pub(super) severity: Severity,
    pub(super) class: Class,
}

/// A defect found by the temper adapter under `rule`.
pub(super) fn finding(rule: Rule, spot: Spot, title: &str, body: &str) -> Finding {
    Finding {
        class: rule.class,
        rule: Some(rule.id.to_owned()),
        kind: FindingKind::Defect,
        severity: rule.severity,
        introduced_by: None,
        pre_existing: false,
        file: spot.file,
        line: spot.line,
        end_line: None,
        side: Some(spot.side),
        evidence: spot.evidence,
        expected_behavior: None,
        trigger: None,
        caller: None,
        title: clip(title, TITLE_CHARS),
        body: clip(body, BODY_CHARS),
        fix: None,
        confidence: None,
        group_hint: None,
    }
}

/// A PR-controlled name (a lint, a crate, a path) made safe to put in a title or inside a
/// markdown code span: anything but ASCII letters, digits and `_:-./` becomes `?`, so no
/// backtick, newline or markup gets through, and `://` becomes `?` so no URL renders as a link
/// where the name is not in a code span. Evidence is quoted verbatim instead.
pub(super) fn plain(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_:-./".contains(c) {
                c
            } else {
                '?'
            }
        })
        .collect::<String>()
        .replace("://", "?")
}

/// `text` cut to its first `max` characters.
pub(super) fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// What was wrong with a JSON document, without quoting it: its text may be PR-controlled, and
/// serde's messages can quote values (the assay-app charter's rule for validation errors).
pub(super) fn json_detail(error: &serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "unreadable",
        serde_json::error::Category::Syntax => "not valid JSON",
        serde_json::error::Category::Data => "not the expected shape",
        serde_json::error::Category::Eof => "truncated",
    };
    format!(
        "{category} at line {} column {}",
        error.line(),
        error.column()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "{\n  \"totals\": {\n    \"a\": {\n      \"x\": 1\n    },\n    \"b\": {\n      \"x\": 2\n    }\n  }\n}\n";

    #[test]
    fn locates_a_key_inside_the_object_named_before_it() {
        let spot = Spot::locate(
            "f.json",
            Side::Right,
            TEXT,
            &["\"totals\"", "\"b\"", "\"x\""],
        );

        assert_eq!(
            spot.map(|spot| (spot.line.get(), spot.evidence)),
            Some((7, "\"x\": 2".to_owned())),
            "the `x` under `b`, not the first `x`"
        );
    }

    #[test]
    fn an_anchor_not_found_after_the_previous_one_locates_nothing() {
        let spot = Spot::locate("f.json", Side::Right, TEXT, &["\"b\"", "\"a\""]);

        assert_eq!(spot, None, "`a` only appears before `b`");
    }

    #[test]
    fn a_name_cannot_break_out_of_a_code_span() {
        assert_eq!(
            plain("clippy::x`\n[link](http://e)"),
            "clippy::x???link??http?e?",
            "backticks, newlines and markup are replaced"
        );
    }

    #[test]
    fn a_name_cannot_render_as_a_link() {
        assert_eq!(
            plain("https://evil.example/x"),
            "https?evil.example/x",
            "GitHub autolinks `scheme://` outside code spans"
        );
    }

    #[test]
    fn clips_by_characters_not_bytes() {
        assert_eq!(
            clip("ééé", 2),
            "éé",
            "a multi-byte character is never split"
        );
    }
}
