//! The deterministic half of coordination: merging exact duplicates, the rule every merge
//! obeys, and the ranking every outcome is published in (04 §4 step 7).

use std::cmp::Reverse;
use std::collections::BTreeMap;

use assay_domain::{Confidence, Fingerprint, Severity};

use super::outcome::{GroupId, Merged, Ranked};
use crate::validation::{Placement, ValidFinding};

/// Whether `duplicate` may be merged into `keeper` without hiding anything: it cites the same
/// file and side, is no more severe, and is pre-existing if the keeper is (a pre-existing
/// finding counts for no rule, 04 §5 rules 6-7).
pub(super) fn may_absorb(keeper: &ValidFinding, duplicate: &ValidFinding) -> bool {
    keeper.file_at_head == duplicate.file_at_head
        && keeper.finding.side == duplicate.finding.side
        && duplicate.finding.severity <= keeper.finding.severity
        && (duplicate.finding.pre_existing || !keeper.finding.pre_existing)
}

/// The findings left once exact duplicates are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Distinct {
    /// The positions of the findings kept, in input order.
    pub(super) kept: Vec<usize>,
    /// The findings merged, in input order.
    pub(super) merged: Vec<Merged>,
}

/// Merges each finding into the finding of its fingerprint that ranks highest (most severe,
/// then not pre-existing, then first), when [`may_absorb`] allows it; a finding it does not
/// allow stays, as its own finding. O(n log n).
pub(super) fn distinct(findings: &[ValidFinding]) -> Distinct {
    let mut keepers: BTreeMap<Fingerprint, (usize, (Severity, bool))> = BTreeMap::new();
    for (index, found) in findings.iter().enumerate() {
        let rank = (found.finding.severity, !found.finding.pre_existing);
        keepers
            .entry(found.fingerprint)
            .and_modify(|keeper| {
                if rank > keeper.1 {
                    *keeper = (index, rank);
                }
            })
            .or_insert((index, rank));
    }
    let mut kept = Vec::with_capacity(findings.len());
    let mut merged = Vec::new();
    for (index, found) in findings.iter().enumerate() {
        let into = keepers
            .get(&found.fingerprint)
            .map(|&(keeper, _)| keeper)
            .filter(|&keeper| keeper != index)
            .filter(|&keeper| findings.get(keeper).is_some_and(|k| may_absorb(k, found)));
        match into {
            Some(into) => merged.push(Merged {
                index,
                into,
                fp: found.fingerprint,
            }),
            None => kept.push(index),
        }
    }
    Distinct { kept, merged }
}

/// A kept finding before ranking: its position, its group in the coordinator's order, and
/// whether it is noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) index: usize,
    pub(super) group: usize,
    pub(super) noise: bool,
}

/// Ranks `entries`, given in the coordinator's order: noise last, then most severe, then
/// surest, then in the order given. Groups are numbered by their first finding in that rank,
/// and noise moves to the summary. O(n log n).
pub(super) fn rank(findings: &[ValidFinding], mut entries: Vec<Entry>) -> Vec<Ranked> {
    // Stable, so equal findings keep the coordinator's order.
    entries.sort_by_key(|entry| {
        let found = findings.get(entry.index).map(|found| &found.finding);
        (
            entry.noise,
            Reverse(found.map(|finding| finding.severity)),
            Reverse(confidence_rank(
                found.and_then(|finding| finding.confidence),
            )),
        )
    });
    let mut numbers: BTreeMap<usize, GroupId> = BTreeMap::new();
    entries
        .into_iter()
        .map(|entry| {
            let next = GroupId::at(numbers.len());
            let group = *numbers.entry(entry.group).or_insert(next);
            let placement = match findings.get(entry.index) {
                Some(found) if !entry.noise => found.placement,
                Some(_) | None => Placement::Summary,
            };
            Ranked {
                index: entry.index,
                group,
                placement,
            }
        })
        .collect()
}

/// The mechanical merge of `findings`: exact duplicates merged, every other finding its own
/// group, ranked in input order otherwise.
pub(super) fn mechanical(findings: &[ValidFinding]) -> (Vec<Ranked>, Vec<Merged>) {
    let Distinct { kept, merged } = distinct(findings);
    let entries = kept
        .into_iter()
        .enumerate()
        .map(|(group, index)| Entry {
            index,
            group,
            noise: false,
        })
        .collect();
    (rank(findings, entries), merged)
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
