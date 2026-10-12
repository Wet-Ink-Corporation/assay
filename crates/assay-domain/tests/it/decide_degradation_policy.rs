//! `decide` rules 8b and 8c of `docs/architecture/04-pipeline.md` §5 for every class and policy
//! combination: a degraded role fails a PR of a class in `block_on_infra_for`, or any PR under
//! `on_infra_failure = block`, and otherwise warns `llm-degraded`.

use std::collections::BTreeSet;
use std::error::Error;

use assay_domain::{
    Conclusion, DegradedRole, InfraPolicy, PrClass, Reason, ReasonCode, RequiredRole, decide,
};

use crate::support::{full_trust_path, reason, rubric_v1};

/// One row of 04 §5 rules 8b and 8c: the PR's classes, the policy, and what a degraded required
/// role gives.
struct Case {
    classes: &'static [PrClass],
    block_on_infra_for: &'static [PrClass],
    on_infra_failure: InfraPolicy,
    fails: bool,
}

const TRUST: PrClass = PrClass::TrustPath;
const EXEC: PrClass = PrClass::ExecSurface;
const WARN: InfraPolicy = InfraPolicy::Warn;
const BLOCK: InfraPolicy = InfraPolicy::Block;

#[rustfmt::skip]
const CASES: [Case; 16] = [
    // The default policy: both classes block, everything else warns.
    Case { classes: &[], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: WARN, fails: false },
    Case { classes: &[TRUST], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: WARN, fails: true },
    Case { classes: &[EXEC], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: WARN, fails: true },
    Case { classes: &[TRUST, EXEC], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: WARN, fails: true },
    // `on_infra_failure = block` fails every class.
    Case { classes: &[], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: BLOCK, fails: true },
    Case { classes: &[TRUST], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: BLOCK, fails: true },
    Case { classes: &[EXEC], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: BLOCK, fails: true },
    Case { classes: &[TRUST, EXEC], block_on_infra_for: &[TRUST, EXEC], on_infra_failure: BLOCK, fails: true },
    // A class blocks only when the base policy lists it.
    Case { classes: &[TRUST], block_on_infra_for: &[EXEC], on_infra_failure: WARN, fails: false },
    Case { classes: &[EXEC], block_on_infra_for: &[TRUST], on_infra_failure: WARN, fails: false },
    Case { classes: &[TRUST, EXEC], block_on_infra_for: &[EXEC], on_infra_failure: WARN, fails: true },
    Case { classes: &[TRUST, EXEC], block_on_infra_for: &[], on_infra_failure: WARN, fails: false },
    Case { classes: &[], block_on_infra_for: &[], on_infra_failure: WARN, fails: false },
    Case { classes: &[], block_on_infra_for: &[], on_infra_failure: BLOCK, fails: true },
    Case { classes: &[TRUST], block_on_infra_for: &[], on_infra_failure: BLOCK, fails: true },
    Case { classes: &[EXEC], block_on_infra_for: &[], on_infra_failure: WARN, fails: false },
];

#[test]
fn a_degraded_role_fails_exactly_the_blocking_classes_and_policies() -> Result<(), Box<dyn Error>> {
    let rubric = rubric_v1()?;
    for (row, case) in CASES.iter().enumerate() {
        let mut inputs = full_trust_path();
        inputs.pr.classes = case.classes.to_vec();
        inputs.policy.block_on_infra_for = case.block_on_infra_for.to_vec();
        inputs.policy.on_infra_failure = case.on_infra_failure;
        inputs.completed_roles = BTreeSet::from([RequiredRole::Security, RequiredRole::Design]);
        inputs.degraded = vec![DegradedRole {
            role: "bugs".to_owned(),
            reason: "cap".to_owned(),
        }];

        let decision = decide(&rubric, &inputs);

        let (conclusion, code) = if case.fails {
            (Conclusion::Failure, ReasonCode::RequiredRoleDegraded)
        } else {
            (Conclusion::Success, ReasonCode::LlmDegraded)
        };
        // A trust-path or exec-surface PR on `full` also requires `meta` (04 §3).
        let meta = (!case.classes.is_empty()).then(|| reason(code, "meta: missing"));
        let expected: Vec<Reason> = [Some(reason(code, "bugs: cap")), meta]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(decision.conclusion, conclusion, "row {row}");
        assert_eq!(decision.reasons, expected, "row {row}");
    }
    Ok(())
}
