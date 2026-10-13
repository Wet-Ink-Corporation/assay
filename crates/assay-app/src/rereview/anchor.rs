//! The deterministic anchor re-check (`docs/architecture/04-pipeline.md` §7): is a carried
//! finding's evidence still at head, and where?

use std::collections::BTreeMap;

use super::carried::CarriedFinding;
use crate::validation::{Change, EvidenceIndex, head_file};

/// Searches for each carried finding's evidence at head with validation step 2's rule (nearest
/// the line last published, the earlier on a tie), following a rename `change` records.
///
/// A finding whose evidence is found moves there ([`CarriedAnchor::AtHead`]); one whose file is
/// gone at head, was not supplied, or no longer holds the evidence stays where it was published
/// ([`CarriedAnchor::Gone`]): it needs re-verification, which is not resolution. Each file is
/// indexed once and searched once per distinct normalised evidence, so the cost is the files'
/// lengths plus one search per distinct quote.
///
/// [`CarriedAnchor::AtHead`]: super::CarriedAnchor::AtHead
/// [`CarriedAnchor::Gone`]: super::CarriedAnchor::Gone
#[must_use]
pub fn recheck_anchors(carried: Vec<CarriedFinding>, change: &Change) -> Vec<CarriedFinding> {
    recheck_with(carried, change, &mut BTreeMap::new())
}

/// [`recheck_anchors`] with its per-file indexes in `indexed`, keyed by the change's own path.
fn recheck_with<'c>(
    carried: Vec<CarriedFinding>,
    change: &'c Change,
    indexed: &mut BTreeMap<&'c str, EvidenceIndex>,
) -> Vec<CarriedFinding> {
    carried
        .into_iter()
        .map(|finding| {
            let (file_at_head, text) = head_file(change, finding.file());
            let found = text.and_then(|(path, content)| {
                indexed
                    .entry(path)
                    .or_insert_with(|| EvidenceIndex::of(content))
                    .nearest(finding.evidence(), finding.line())
            });
            match found {
                Some(line) => finding.at_head(file_at_head, line),
                None => finding.gone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::num::NonZeroU64;

    use assay_domain::{Class, Severity};

    use super::recheck_with;
    use crate::rereview::{CarriedAnchor, CarriedFinding, CarriedParts, MAX_CARRIED};
    use crate::validation::Change;

    /// Review F6: findings quoting the same evidence search their file once, whatever their
    /// number.
    #[test]
    fn carried_findings_sharing_evidence_search_their_file_once() {
        let change = Change {
            head_files: BTreeMap::from([(
                "src/lib.rs".to_owned(),
                "fn f() {\n    for i in 0..=items.len() {\n".to_owned(),
            )]),
            ..Change::default()
        };
        let carried: Vec<CarriedFinding> = (0..MAX_CARRIED)
            .map(|n| {
                CarriedFinding::read(
                    CarriedParts {
                        fp: format!("{n:016x}").parse().expect("16 hex digits"),
                        file: "src/lib.rs".to_owned(),
                        line: NonZeroU64::MIN,
                        class: Class::Logic,
                        severity: Severity::Major,
                        title: "off by one".to_owned(),
                        trigger: None,
                        evidence: "for i in 0..=items.len() {".to_owned(),
                    },
                    "owner/repo",
                )
                .expect("a well-formed carried finding")
            })
            .collect();
        let mut indexed = BTreeMap::new();

        let rechecked = recheck_with(carried, &change, &mut indexed);

        assert!(
            rechecked
                .iter()
                .all(|found| found.anchor() == CarriedAnchor::AtHead && found.line().get() == 2)
        );
        let searches: Vec<usize> = indexed
            .values()
            .map(super::EvidenceIndex::searches)
            .collect();
        assert_eq!(searches, [1]);
    }
}
