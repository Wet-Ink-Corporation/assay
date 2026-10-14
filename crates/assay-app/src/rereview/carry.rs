//! The carry set a run hands to the next (`docs/architecture/04-pipeline.md` §7, #224): its
//! carried findings still open after this run, as last verified, and this run's own findings
//! that stand, recorded in its `assay.run-inputs/1` bundle so a finding stays open until a run
//! re-judges it, however many runs that takes.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use assay_domain::{Fingerprint, Severity, VerifierVerdict};

use super::carried::{CarriedFinding, CarriedParts, CarriedRefusal, MAX_CARRIED};
use super::settle::{CarriedState, settle_carried};
use crate::validation::ValidFinding;
use crate::verify::{ClaimRef, Verification, VerifiedFinding};

/// The findings a run carries into the next one, at most [`MAX_CARRIED`], one per fingerprint,
/// most severe first; and the open findings it could not carry, each named by fingerprint.
///
/// Built by [`next_carry`] or [`CarrySet::unjudged`]; every finding in it passed
/// [`CarriedFinding::read`], so the next run's intake reads back exactly what was written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CarrySet {
    findings: Vec<CarriedFinding>,
    omitted: Vec<Omitted>,
}

/// An open finding the carry set does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Omitted {
    /// Its fingerprint.
    pub fp: Fingerprint,
    /// Why it is not carried.
    pub reason: OmitReason,
}

/// Why an open finding is not carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OmitReason {
    /// [`MAX_CARRIED`] more severe findings fill the set.
    Cap,
    /// The carried intake would refuse it, so the next run would refuse the whole set.
    Refused(CarriedRefusal),
}

impl CarrySet {
    /// The carry set of a run that judged none of `carried` (its review stage did not run):
    /// each stays as last verified, ranked and capped as [`next_carry`] ranks them.
    #[must_use]
    pub fn unjudged(carried: Vec<CarriedFinding>) -> Self {
        let mut gathered = Gathered::default();
        for finding in carried {
            gathered.add(finding);
        }
        gathered.finish()
    }

    /// The findings carried, most severe first, then carried before this run's own, then in
    /// the order given.
    #[must_use]
    pub fn findings(&self) -> &[CarriedFinding] {
        &self.findings
    }

    /// The open findings left out: refusals first, by fingerprint, then those past the cap in
    /// rank order.
    #[must_use]
    pub fn omitted(&self) -> &[Omitted] {
        &self.omitted
    }
}

/// The carry set after this run, for a change to `repository` (`owner/name`).
///
/// `carried` is settled against `verification` and `raised` as [`settle_carried`] settles
/// them; `raised` is this run's validated findings, the slice `verification` was computed over.
/// A carried finding resolved this run leaves the set, and one this run raised again is carried
/// as the fresh finding; one confirmed or unclear carries this run's severity and verdict; one
/// not re-verified (cap, a failed verifier, `micro`) stays as last verified. Its location is
/// where its evidence is at head when [`recheck_anchors`](super::recheck_anchors) found it, else
/// where it was last published. A fresh finding the verifier refuted is not carried; any other
/// is carried at its head path with its verified severity and verdict, or its validated
/// severity and no verdict when it was not judged. Where one fingerprint is both carried and
/// raised, or raised twice, the latest text is kept at the higher severity (and that
/// severity's verdict), so a re-raise cannot lower a carried finding without the verifier.
#[must_use]
pub fn next_carry<E>(
    carried: Vec<CarriedFinding>,
    raised: &[ValidFinding],
    verification: &Verification<E>,
    repository: &str,
) -> CarrySet {
    let outcomes = settle_carried(&carried, raised, verification);
    let mut gathered = Gathered::default();
    for (finding, outcome) in carried.into_iter().zip(outcomes) {
        match outcome.state {
            CarriedState::Resolved { .. } | CarriedState::Raised => {}
            CarriedState::Open => gathered.add(finding.judged(outcome.severity, outcome.verdict)),
            CarriedState::NotReverified(_) => gathered.add(finding),
        }
    }
    for (index, found) in raised.iter().enumerate() {
        let verified = verification
            .findings
            .get(index)
            .filter(|verified| verified.at == ClaimRef::Raised(found.at));
        let Some((severity, verdict)) = standing(found, verified) else {
            continue;
        };
        match CarriedFinding::read(fresh_parts(found, severity, verdict), repository) {
            Ok(fresh) => gathered.add(fresh),
            Err(refusal) => gathered.refuse(found.fingerprint, refusal),
        }
    }
    gathered.finish()
}

/// The severity and verdict `found` is carried with, or `None` when the verifier refuted it.
fn standing(
    found: &ValidFinding,
    verified: Option<&VerifiedFinding>,
) -> Option<(Severity, Option<VerifierVerdict>)> {
    match verified.map(|verified| (verified.severity, verified.verdict())) {
        Some((_, Some(VerifierVerdict::Refuted))) => None,
        Some((severity, Some(verdict))) => Some((severity, Some(verdict))),
        Some((_, None)) | None => Some((found.finding.severity, None)),
    }
}

/// `found` as a carried finding: where it is at head, with the tree's lines it matched as its
/// evidence. The strings are copied, since `found` still feeds the decision.
fn fresh_parts(
    found: &ValidFinding,
    severity: Severity,
    verdict: Option<VerifierVerdict>,
) -> CarriedParts {
    CarriedParts {
        fp: found.fingerprint,
        file: found.file_at_head.clone(),
        line: found.finding.line,
        end_line: found.finding.end_line,
        class: found.finding.class,
        severity,
        verdict,
        title: found.finding.title.clone(),
        trigger: found.finding.trigger.clone(),
        evidence: found.finding.evidence.clone(),
    }
}

/// The carry set being built: findings in insertion order, one per fingerprint.
#[derive(Default)]
struct Gathered {
    findings: Vec<CarriedFinding>,
    at: BTreeMap<Fingerprint, usize>,
    refused: BTreeMap<Fingerprint, CarriedRefusal>,
}

impl Gathered {
    /// Adds `finding`, or merges it into the one already held for its fingerprint: the later
    /// text, at the higher severity with that severity's verdict.
    fn add(&mut self, finding: CarriedFinding) {
        let Some(&index) = self.at.get(&finding.fp()) else {
            self.at.insert(finding.fp(), self.findings.len());
            self.findings.push(finding);
            return;
        };
        if let Some(held) = self.findings.get_mut(index) {
            let merged = if held.severity() > finding.severity() {
                finding.judged(held.severity(), held.verdict())
            } else {
                finding
            };
            *held = merged;
        }
    }

    /// Records that the open finding `fp` could not be carried (its first refusal).
    fn refuse(&mut self, fp: Fingerprint, refusal: CarriedRefusal) {
        self.refused.entry(fp).or_insert(refusal);
    }

    /// Ranks the findings (a stable sort, so ties keep insertion order), keeps the first
    /// [`MAX_CARRIED`] and lists the rest; a refusal whose fingerprint is held anyway is not
    /// listed as refused.
    fn finish(self) -> CarrySet {
        let Self {
            mut findings,
            at,
            refused,
        } = self;
        findings.sort_by_key(|finding| Reverse(finding.severity()));
        let past_cap = findings.split_off(findings.len().min(MAX_CARRIED));
        let omitted = refused
            .into_iter()
            .filter(|(fp, _)| !at.contains_key(fp))
            .map(|(fp, refusal)| Omitted {
                fp,
                reason: OmitReason::Refused(refusal),
            })
            .chain(past_cap.iter().map(|finding| Omitted {
                fp: finding.fp(),
                reason: OmitReason::Cap,
            }))
            .collect();
        CarrySet { findings, omitted }
    }
}
