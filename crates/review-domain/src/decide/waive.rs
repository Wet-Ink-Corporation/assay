//! Waive (`docs/architecture/04-pipeline.md` §5): an owner's `/assay waive <finding-id>` removes
//! one human-decision gate finding from rule 1.
//!
//! A fingerprint names a finding, not a gate or a rule, and two findings can share one (a temper
//! `exception` and a clippy finding on the same line). A waiver therefore applies all or
//! nothing: it clears its fingerprint only when every gate finding carrying it, in every gate,
//! has a rule the rubric lists as waivable for that gate. Otherwise it clears none of them, so
//! what a decision counts and what it reports `waived` are always the same findings.

use std::collections::{BTreeMap, BTreeSet};

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
    waiver_refusal(rubric, gates, fp).is_none()
}

/// Why an owner's waiver of a fingerprint clears nothing (R-08(a), 04 §5 waive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaiverRefusal<'g> {
    /// No gate finding of the decided head carries the fingerprint: it names a reviewer
    /// finding, a finding of another head, or nothing.
    NoGateFinding,
    /// A gate finding carrying it is not a human-decision finding the rubric lists as waivable
    /// for its gate (a clippy, build, test or deny finding, or one with no rule): the first
    /// such finding, in gate then finding order.
    NotWaivable {
        /// The gate (plugin) that reported it.
        gate: &'g str,
        /// Its rule id, if it has one.
        rule: Option<&'g str>,
    },
}

/// Why a waiver of `fp` would clear nothing over `gates`, or `None` when `decide` applies it
/// (given an actor): the same all-or-nothing predicate as `decide`'s waive and
/// [`waivable_fingerprints`].
///
/// The refusal borrows the gate name and rule it reports from `gates`.
#[must_use]
pub fn waiver_refusal<'g>(
    rubric: &Rubric,
    gates: &'g [GateReport],
    fp: Fingerprint,
) -> Option<WaiverRefusal<'g>> {
    let mut carriers = gates
        .iter()
        .flat_map(|gate| {
            gate.findings
                .iter()
                .filter(move |finding| finding.fp == fp)
                .map(move |finding| (gate, finding))
        })
        .peekable();
    if carriers.peek().is_none() {
        return Some(WaiverRefusal::NoGateFinding);
    }
    carriers
        .find(|(gate, finding)| !waivable(rubric, gate, finding))
        .map(|(gate, finding)| WaiverRefusal::NotWaivable {
            gate: gate.name.as_str(),
            rule: finding.rule.as_deref(),
        })
}

/// The fingerprints an owner's `/assay waive` may clear (R-08(a), 04 §5 waive).
///
/// Those are the fingerprints carried by at least one gate finding and only by findings whose
/// rule the rubric lists as waivable for their own gate, the rule a waiver is applied by.
/// Reviewer findings are never waivable. One pass over the gate findings, `O(n log n)`.
#[must_use]
pub fn waivable_fingerprints(rubric: &Rubric, gates: &[GateReport]) -> BTreeSet<Fingerprint> {
    let mut all_waivable: BTreeMap<Fingerprint, bool> = BTreeMap::new();
    for gate in gates {
        for finding in &gate.findings {
            let this = waivable(rubric, gate, finding);
            all_waivable
                .entry(finding.fp)
                .and_modify(|every| *every &= this)
                .or_insert(this);
        }
    }
    all_waivable
        .into_iter()
        .filter_map(|(fp, every)| every.then_some(fp))
        .collect()
}

fn waivable(rubric: &Rubric, gate: &GateReport, finding: &GateFinding) -> bool {
    finding
        .rule
        .as_deref()
        .is_some_and(|rule| rubric.is_waivable(&gate.name, rule))
}
