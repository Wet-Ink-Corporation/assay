//! The waivers a decision did not apply, each with why, for the verdict summary (R-08(a), #59):
//! a waiver of a clippy, build, test or deny finding is refused visibly, not silently.

use std::collections::BTreeSet;

use assay_domain::{Decision, DecisionInputs, Fingerprint, Rubric, WaiverRefusal, waiver_refusal};

/// One line per fingerprint `inputs` asked to waive that `decision` did not, in input order:
/// `<fp> by <who>: <why>`, or `<fp>: <why>` for a waiver with no recorded owner. The first
/// waiver of a fingerprint names who.
pub(super) fn unapplied(
    rubric: &Rubric,
    inputs: &DecisionInputs,
    decision: &Decision,
) -> Vec<String> {
    let applied: BTreeSet<Fingerprint> = decision.waived.iter().map(|waived| waived.fp).collect();
    let mut seen = BTreeSet::new();
    inputs
        .waivers
        .iter()
        .filter(|waiver| !applied.contains(&waiver.fp) && seen.insert(waiver.fp))
        .map(|waiver| {
            let why = match waiver_refusal(rubric, &inputs.gates, waiver.fp) {
                Some(WaiverRefusal::NoGateFinding) => {
                    "no gate finding of this head carries it".to_owned()
                }
                Some(WaiverRefusal::NotWaivable {
                    gate,
                    rule: Some(rule),
                }) => format!("{gate} rule {rule} is not a human-decision finding"),
                Some(WaiverRefusal::NotWaivable { gate, rule: None }) => {
                    format!("a {gate} finding without a rule is not a human-decision finding")
                }
                // Waivable, so only a waiver with no recorded actor is left unapplied.
                None => "no eligible owner is recorded for it".to_owned(),
            };
            if waiver.by.is_empty() {
                format!("{}: {why}", waiver.fp)
            } else {
                format!("{} by {}: {why}", waiver.fp, waiver.by)
            }
        })
        .collect()
}
