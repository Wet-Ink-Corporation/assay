//! Waive (`docs/architecture/04-pipeline.md` §5): an owner's `/assay waive <finding-id>` removes
//! one human-decision gate finding from rule 1.
//!
//! A fingerprint names a finding, not a gate or a rule, and two findings can share one (a temper
//! `exception` and a clippy finding on the same line). So a waiver is resolved to the
//! (gate, fingerprint) pairs of findings whose own rule the rubric lists as waivable, and a
//! finding counts as waived only if its own rule is waivable and its own pair was resolved.

use std::collections::BTreeMap;

use crate::decide::inputs::{DecisionInputs, GateFinding, GateReport};
use crate::decision::Waived;
use crate::rubric::Rubric;

/// The waivers of one decision, resolved against its gates.
pub(super) struct Waivers<'a> {
    rubric: &'a Rubric,
    /// (gate name, fingerprint) of each waived finding, and who waived it.
    applied: BTreeMap<(&'a str, &'a str), &'a str>,
}

impl<'a> Waivers<'a> {
    /// Resolves the inputs' waivers. A waiver with an empty actor is not applied: it could not
    /// be recorded in the decision's `waived` list.
    pub(super) fn resolve(rubric: &'a Rubric, inputs: &'a DecisionInputs) -> Self {
        let mut applied = BTreeMap::new();
        for gate in &inputs.gates {
            for finding in &gate.findings {
                if !waivable(rubric, gate, finding) {
                    continue;
                }
                let by = inputs
                    .waivers
                    .iter()
                    .find(|waiver| waiver.fp == finding.fp && !waiver.by.is_empty());
                if let Some(waiver) = by {
                    applied
                        .entry((gate.name.as_str(), finding.fp.as_str()))
                        .or_insert(waiver.by.as_str());
                }
            }
        }
        Self { rubric, applied }
    }

    /// Whether `finding` of `gate` is waived: its own rule is waivable and its own
    /// (gate, fingerprint) pair was waived.
    pub(super) fn covers(&self, gate: &GateReport, finding: &GateFinding) -> bool {
        waivable(self.rubric, gate, finding)
            && self
                .applied
                .contains_key(&(gate.name.as_str(), finding.fp.as_str()))
    }

    /// The decision's `waived` list: one entry per waived fingerprint, sorted.
    pub(super) fn waived(&self) -> Vec<Waived> {
        let by_fp: BTreeMap<&str, &str> = self
            .applied
            .iter()
            .map(|(&(_, fp), &by)| (fp, by))
            .collect();
        by_fp
            .into_iter()
            .map(|(fp, by)| Waived {
                fp: fp.to_owned(),
                by: by.to_owned(),
            })
            .collect()
    }
}

fn waivable(rubric: &Rubric, gate: &GateReport, finding: &GateFinding) -> bool {
    finding
        .rule
        .as_deref()
        .is_some_and(|rule| rubric.is_waivable(&gate.name, rule))
}
