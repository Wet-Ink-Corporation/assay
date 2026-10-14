//! Which validated findings become inline comments (04 §4 step 7, 04 §6).

use std::cmp::Reverse;
use std::collections::BTreeSet;

use assay_domain::Confidence;

use super::MAX_INLINE_COMMENTS;
use crate::validation::{Placement, ValidFinding};

/// The findings published inline, at most [`MAX_INLINE_COMMENTS`]: those placed inline, most
/// severe first, then by confidence (high, medium, low, unstated), then in input order; one per
/// fingerprint, its most severe copy. O(n log n) in the findings.
pub(super) fn select(findings: &[ValidFinding]) -> Vec<&ValidFinding> {
    let mut inline: Vec<&ValidFinding> = findings
        .iter()
        .filter(|found| found.placement == Placement::Inline)
        .collect();
    // Stable, so equal findings keep their input order.
    inline.sort_by_key(|found| {
        (
            Reverse(found.finding.severity),
            Reverse(confidence_rank(found.finding.confidence)),
        )
    });
    let mut seen = BTreeSet::new();
    inline.retain(|found| seen.insert(found.fingerprint));
    inline.truncate(MAX_INLINE_COMMENTS);
    inline
}

/// Higher for surer findings; an unstated confidence ranks lowest.
const fn confidence_rank(confidence: Option<Confidence>) -> u8 {
    match confidence {
        Some(Confidence::High) => 3,
        Some(Confidence::Medium) => 2,
        Some(Confidence::Low) => 1,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use assay_domain::{Confidence, Fingerprint, Severity};

    use super::select;
    use crate::publish::findings::MAX_INLINE_COMMENTS;
    use crate::publish::findings::fixture::valid;
    use crate::validation::{Placement, ValidFinding};

    fn fingerprints(selected: &[&ValidFinding]) -> Vec<Fingerprint> {
        selected.iter().map(|found| found.fingerprint).collect()
    }

    fn fp(value: u64) -> Fingerprint {
        format!("{value:016x}").parse().expect("16 hex digits")
    }

    #[test]
    fn blockers_come_first_then_majors_by_confidence() {
        let findings = [
            valid(1, Severity::Minor, Some(Confidence::High)).expect("fixture"),
            valid(2, Severity::Major, Some(Confidence::Low)).expect("fixture"),
            valid(3, Severity::Major, None).expect("fixture"),
            valid(4, Severity::Blocker, Some(Confidence::Low)).expect("fixture"),
            valid(5, Severity::Major, Some(Confidence::High)).expect("fixture"),
            valid(6, Severity::Nit, Some(Confidence::High)).expect("fixture"),
            valid(7, Severity::Major, Some(Confidence::Medium)).expect("fixture"),
        ];

        let selected = select(&findings);

        assert_eq!(
            fingerprints(&selected),
            [4, 5, 7, 2, 3, 1, 6].map(fp),
            "blocker, majors high > medium > low > unstated, minor, nit"
        );
    }

    #[test]
    fn equal_findings_keep_their_input_order() {
        let findings = [
            valid(9, Severity::Major, Some(Confidence::High)).expect("fixture"),
            valid(8, Severity::Major, Some(Confidence::High)).expect("fixture"),
        ];

        assert_eq!(fingerprints(&select(&findings)), [9, 8].map(fp));
    }

    #[test]
    fn the_cap_of_inline_findings_is_kept_whole() {
        let findings: Vec<ValidFinding> = (1..=8)
            .map(|n| valid(n, Severity::Major, None).expect("fixture"))
            .collect();

        assert_eq!(select(&findings).len(), MAX_INLINE_COMMENTS);
    }

    #[test]
    fn findings_past_the_cap_are_not_inline() {
        let findings: Vec<ValidFinding> = (1..=9)
            .map(|n| valid(n, Severity::Major, None).expect("fixture"))
            .collect();

        assert_eq!(
            fingerprints(&select(&findings)),
            (1..=8).map(fp).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_fingerprint_is_selected_once() {
        let findings = [
            valid(1, Severity::Minor, None).expect("fixture"),
            valid(1, Severity::Blocker, None).expect("fixture"),
            valid(2, Severity::Nit, None).expect("fixture"),
        ];

        let selected = select(&findings);

        assert_eq!(fingerprints(&selected), [1, 2].map(fp));
        assert_eq!(
            selected.first().map(|found| found.finding.severity),
            Some(Severity::Blocker),
            "the most severe copy is the one published"
        );
    }

    #[test]
    fn summary_findings_are_not_inline() {
        let mut moved = valid(1, Severity::Blocker, None).expect("fixture");
        moved.placement = Placement::Summary;
        let findings = [moved, valid(2, Severity::Nit, None).expect("fixture")];

        assert_eq!(fingerprints(&select(&findings)), [fp(2)]);
    }
}
