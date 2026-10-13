//! Waive (`docs/architecture/04-pipeline.md` §5): an owner's `/assay waive <finding-id>` removes
//! one human-decision gate finding from rule 1.
//!
//! A fingerprint names a finding, not a gate or a rule, and two findings can share one (a temper
//! `exception` and a clippy finding on the same line). A waiver therefore applies all or
//! nothing: it clears its fingerprint only when every gate finding carrying it, in every gate,
//! has a rule the rubric lists as waivable for that gate. Otherwise it clears none of them, so
//! what a decision counts and what it reports `waived` are always the same findings.

use std::collections::BTreeMap;

use crate::decide::inputs::{DecisionInputs, GateFinding, GateReport};
use crate::decision::Waived;
use crate::fingerprint::Fingerprint;
use crate::rubric::Rubric;

/// The waivers of one decision, resolved against its gates.
pub(super) struct Waivers<'a> {
    /// Each applied fingerprint and who waived it. Every gate finding carrying one of these is
    /// waivable for its own gate.
    applied: BTreeMap<Fingerprint, &'a str>,
}

impl<'a> Waivers<'a> {
    /// Resolves the inputs' waivers. A waiver is applied only if it has an actor (it could not
    /// be recorded in the decision's `waived` list otherwise) and its fingerprint is carried by
    /// at least one gate finding and only by waivable ones. The first applicable waiver of a
    /// fingerprint names who waived it.
    pub(super) fn resolve(rubric: &Rubric, inputs: &'a DecisionInputs) -> Self {
        let mut applied = BTreeMap::new();
        for waiver in &inputs.waivers {
            if !waiver.by.is_empty() && clearable(rubric, &inputs.gates, waiver.fp) {
                applied.entry(waiver.fp).or_insert(waiver.by.as_str());
            }
        }
        Self { applied }
    }

    /// Whether `finding` is waived: a waiver applied to its fingerprint.
    pub(super) fn covers(&self, finding: &GateFinding) -> bool {
        self.applied.contains_key(&finding.fp)
    }

    /// The decision's `waived` list: one entry per applied fingerprint, sorted.
    pub(super) fn waived(&self) -> Vec<Waived> {
        self.applied
            .iter()
            .map(|(&fp, &by)| Waived {
                fp,
                by: by.to_owned(),
            })
            .collect()
    }
}

/// Whether a waiver of `fp` may clear it: some gate finding carries it, and every one that does
/// has a rule the rubric lists as waivable for its own gate.
fn clearable(rubric: &Rubric, gates: &[GateReport], fp: Fingerprint) -> bool {
    let mut carriers = gates
        .iter()
        .flat_map(|gate| {
            gate.findings
                .iter()
                .filter(move |finding| finding.fp == fp)
                .map(move |finding| (gate, finding))
        })
        .peekable();
    carriers.peek().is_some() && carriers.all(|(gate, finding)| waivable(rubric, gate, finding))
}

fn waivable(rubric: &Rubric, gate: &GateReport, finding: &GateFinding) -> bool {
    finding
        .rule
        .as_deref()
        .is_some_and(|rule| rubric.is_waivable(&gate.name, rule))
}
