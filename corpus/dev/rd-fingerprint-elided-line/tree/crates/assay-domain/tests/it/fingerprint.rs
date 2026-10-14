//! Fingerprints and the fuzzy dismissal match, through the public API (03 §3, issue #49).

use std::error::Error;

use assay_domain::{
    Anchor, Class, Confidence, Dismissal, DismissalMatch, Finding, FindingsDocument, Severity,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;

use crate::support::valid_fixtures;

/// The first finding of the reviewer fixture: a `major` on `let x = y.unwrap();`.
fn reviewer_finding() -> Result<Finding, Box<dyn Error>> {
    let fixtures = valid_fixtures("findings")?;
    let bytes = fixtures.get("reviewer").ok_or("no reviewer fixture")?;
    let document: FindingsDocument = serde_json::from_slice(bytes)?;
    Ok(document.findings.into_iter().next().ok_or("no finding")?)
}

#[test]
fn two_differently_worded_findings_on_the_same_defect_and_class_collide()
-> Result<(), Box<dyn Error>> {
    let first = reviewer_finding()?;
    let second = Finding {
        severity: Severity::Blocker,
        line: first.line.saturating_add(1),
        evidence: "let  x = y.unwrap();   // panics on an empty body".to_owned(),
        trigger: Some("an empty request body".to_owned()),
        title: "Handler panics when the body is empty".to_owned(),
        body: "`unwrap` on a value the client controls.".to_owned(),
        fix: None,
        confidence: Some(Confidence::Medium),
        group_hint: None,
        ..first.clone()
    };

    let fingerprints = [&first, &second].map(|finding| {
        finding
            .anchor("crates/x/src/lib.rs", "parse")
            .fingerprint(0)
    });

    assert_eq!(fingerprints[0], fingerprints[1]);
    Ok(())
}

#[test]
fn the_anchor_takes_the_head_path_given_not_the_finding_s_own() -> Result<(), Box<dyn Error>> {
    let finding = reviewer_finding()?;

    let anchor = finding.anchor("crates/x/src/renamed.rs", "parse");

    assert_eq!(
        anchor,
        Anchor {
            class: Class::ErrorHandling,
            file: "crates/x/src/renamed.rs",
            enclosing_symbol: "parse",
            evidence: "let x = y.unwrap();",
        }
    );
    Ok(())
}

/// Every quote style the normaliser unifies.
const QUOTES: [&str; 7] = [
    "\"", "'", "`", "\u{2018}", "\u{2019}", "\u{201c}", "\u{201d}",
];

/// An anchor on `evidence` in `parse`.
fn in_parse(evidence: &str) -> Anchor<'_> {
    Anchor {
        class: Class::Logic,
        file: "src/lib.rs",
        enclosing_symbol: "parse",
        evidence,
    }
}

proptest! {
    // Line breaks may surround the line but not split it: only the first line counts (03 §3).
    #[test]
    fn fingerprint_is_stable_under_whitespace_and_quote_style_changes(
        words in vec("[A-Za-z0-9_(){};=,.:]{1,6}", 1..8),
        quoted in vec(any::<bool>(), 8),
        styles in vec(select(QUOTES.to_vec()), 8),
        gaps in vec("[ \t]{1,3}", 8),
        ends in ("[ \t\n]{0,3}", "[ \t\n]{0,3}"),
        occurrence in 0..4_u32,
    ) {
        let mut plain = Vec::new();
        let mut restyled = ends.0;
        for (((word, quoted), style), gap) in words.iter().zip(&quoted).zip(&styles).zip(&gaps) {
            let (plain_quote, other_quote) = if *quoted { ("\"", *style) } else { ("", "") };
            plain.push(format!("{plain_quote}{word}{plain_quote}"));
            restyled.extend([other_quote, word.as_str(), other_quote, gap.as_str()]);
        }
        restyled.push_str(&ends.1);
        let plain = plain.join(" ");

        prop_assert_eq!(
            in_parse(&restyled).fingerprint(occurrence),
            in_parse(&plain).fingerprint(occurrence),
            "{:?} vs {:?}", restyled, plain
        );
    }
}

/// A dismissal of a `major` whose evidence is 20 characters long.
fn dismissed_major() -> Dismissal<'static> {
    Dismissal {
        anchor: in_parse("call(abcdefghijklmn)"),
        severity: Severity::Major,
    }
}

#[test]
fn a_dismissal_matches_evidence_exactly_four_fifths_similar() {
    // Four of twenty characters differ: similarity 0.8.
    let outcome = dismissed_major().judge(&in_parse("call(abcdefghijWXYZ)"), Severity::Major);

    assert_eq!(outcome, DismissalMatch::Suppressed);
}

#[test]
fn a_dismissal_does_not_match_evidence_below_four_fifths_similar() {
    // Five of twenty characters differ: similarity 0.75.
    let outcome = dismissed_major().judge(&in_parse("call(abcdefghiVWXYZ)"), Severity::Major);

    assert_eq!(outcome, DismissalMatch::Unrelated);
}
