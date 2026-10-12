//! Fingerprints and the fuzzy dismissal match, through the public API (03 §3, issue #49).

use std::error::Error;

use assay_domain::{
    Anchor, Class, Confidence, Dismissal, DismissalMatch, Finding, FindingKind, FindingsDocument,
    Severity,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;

use crate::support::valid_fixtures;

/// The findings of the reviewer fixture: a `major` defect on `let x = y.unwrap();`, then a
/// `minor` omission anchored on `parse`'s signature.
fn reviewer_findings() -> Result<Vec<Finding>, Box<dyn Error>> {
    let fixtures = valid_fixtures("findings")?;
    let bytes = fixtures.get("reviewer").ok_or("no reviewer fixture")?;
    let document: FindingsDocument = serde_json::from_slice(bytes)?;
    Ok(document.findings)
}

/// The defect of the reviewer fixture.
fn reviewer_finding() -> Result<Finding, Box<dyn Error>> {
    Ok(reviewer_findings()?
        .into_iter()
        .next()
        .ok_or("no finding")?)
}

/// The omission of the reviewer fixture.
fn reviewer_omission() -> Result<Finding, Box<dyn Error>> {
    Ok(reviewer_findings()?
        .into_iter()
        .find(|finding| finding.kind == FindingKind::Omission)
        .ok_or("no omission")?)
}

#[test]
fn an_omission_s_anchor_carries_its_expected_behavior() -> Result<(), Box<dyn Error>> {
    let omission = reviewer_omission()?;

    let anchor = omission.anchor("crates/x/src/lib.rs", "parse");

    assert_eq!(
        anchor.expected_behavior,
        Some("an empty input is rejected with ParseError::Empty")
    );
    Ok(())
}

#[test]
fn an_omission_without_expected_behavior_anchors_on_an_empty_one() -> Result<(), Box<dyn Error>> {
    let unvalidated = Finding {
        expected_behavior: None,
        ..reviewer_omission()?
    };

    let anchor = unvalidated.anchor("crates/x/src/lib.rs", "parse");

    assert_eq!(anchor.expected_behavior, Some(""));
    Ok(())
}

#[test]
fn a_defect_s_anchor_ignores_any_expected_behavior() -> Result<(), Box<dyn Error>> {
    let defect = Finding {
        expected_behavior: Some("no panic".to_owned()),
        ..reviewer_finding()?
    };

    assert_eq!(
        defect
            .anchor("crates/x/src/lib.rs", "parse")
            .expected_behavior,
        None
    );
    Ok(())
}

#[test]
fn two_omissions_in_one_function_with_different_expectations_do_not_collide()
-> Result<(), Box<dyn Error>> {
    let empty = reviewer_omission()?;
    let oversized = Finding {
        expected_behavior: Some("an input over 1 MiB is rejected before parsing".to_owned()),
        ..empty.clone()
    };
    let [empty, oversized] =
        [&empty, &oversized].map(|finding| finding.anchor("crates/x/src/lib.rs", "parse"));

    assert_ne!(empty.fingerprint(0), oversized.fingerprint(0));
    assert_eq!(
        Dismissal {
            anchor: empty,
            severity: Severity::Blocker
        }
        .judge(&oversized, Severity::Nit),
        DismissalMatch::Unrelated
    );
    Ok(())
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

    let [first, second] = [&first, &second].map(|finding| {
        finding
            .anchor("crates/x/src/lib.rs", "parse")
            .fingerprint(0)
    });

    assert_eq!(Some(first.ok_or("the defect has content")?), second);
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
            expected_behavior: None,
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
        expected_behavior: None,
    }
}

proptest! {
    // Lines from disjoint alphabets are never 0.8 similar, so neither the fingerprint nor the
    // fuzzy match may let a shared elision make them one finding (review F1, F2).
    #[test]
    fn different_lines_behind_a_shared_elision_stay_distinct(
        elision in select(vec!["...", "\u{2026}", "... \u{2026}", "  ...  "]),
        left in "[a-m]{4,12}\\(\\);",
        right in "[n-z]{4,12}\\(\\);",
    ) {
        let left_evidence = format!("{elision}\n{left}");
        let right_evidence = format!("{elision}\n{right}");
        let (left, right) = (in_parse(&left_evidence), in_parse(&right_evidence));

        prop_assert_ne!(left.fingerprint(0), right.fingerprint(0));
        prop_assert_eq!(
            Dismissal { anchor: left, severity: Severity::Blocker }.judge(&right, Severity::Nit),
            DismissalMatch::Unrelated
        );
    }

    // Line breaks may surround the line but not split it: only the first line counts (03 §3).
    // Words like `...` can leave no content; both sides then have no fingerprint, which is
    // stable too.
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
