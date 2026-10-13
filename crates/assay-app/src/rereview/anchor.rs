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
/// indexed once, so the cost is the files' lengths plus one search per finding.
///
/// [`CarriedAnchor::AtHead`]: super::CarriedAnchor::AtHead
/// [`CarriedAnchor::Gone`]: super::CarriedAnchor::Gone
#[must_use]
pub fn recheck_anchors(carried: Vec<CarriedFinding>, change: &Change) -> Vec<CarriedFinding> {
    let mut indexed: BTreeMap<String, Option<EvidenceIndex>> = BTreeMap::new();
    carried
        .into_iter()
        .map(|finding| {
            let (file_at_head, text) = head_file(change, finding.file());
            // The memo and the moved finding each keep the path.
            let index = indexed
                .entry(file_at_head.clone())
                .or_insert_with(|| text.map(EvidenceIndex::of));
            let found = index
                .as_ref()
                .and_then(|index| index.nearest(finding.evidence(), finding.line()));
            match found {
                Some(line) => finding.at_head(file_at_head, line),
                None => finding.gone(),
            }
        })
        .collect()
}
