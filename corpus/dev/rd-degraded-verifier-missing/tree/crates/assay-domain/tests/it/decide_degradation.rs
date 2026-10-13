//! `decide` rules 8b and 8c of `docs/architecture/04-pipeline.md` §5: a required role that did
//! not complete, a failed coordinator or an unavailable verifier degrades the LLM stage, which
//! fails a PR of a blocking class or policy and otherwise warns, never silently (04 §3, 06 §6).

use std::error::Error;

use assay_domain::{
    Conclusion, Decision, DecisionInputs, Deferral, DegradedRole, InfraPolicy, LlmStage,
    OwnerApproval, PrClass, Reason, ReasonCode, RequiredRole, Severity, SupportOutcome, Tier,
    decide,
};

use crate::support::{clean_inputs, confirmed, reason, rubric_v1};

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

fn degraded(role: &str, reason: &str) -> DegradedRole {
    DegradedRole {
        role: role.to_owned(),
        reason: reason.to_owned(),
    }
}

#[test]
fn a_lite_pr_whose_required_role_never_completed_warns_llm_degraded() -> Result<(), Box<dyn Error>>
{
    let mut inputs = clean_inputs();
    inputs.completed_roles = vec![RequiredRole::Bugs];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "combined: missing")]
    );
    assert_eq!(decision.degraded, [degraded("combined", "missing")]);
    Ok(())
}

#[test]
fn a_full_trust_path_pr_missing_meta_fails_required_role_degraded() -> Result<(), Box<dyn Error>> {
    let mut inputs = full_trust_path();
    inputs.completed_roles = vec![
        RequiredRole::Bugs,
        RequiredRole::Security,
        RequiredRole::Design,
    ];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::RequiredRoleDegraded, "meta: missing")]
    );
    assert_eq!(decision.degraded, [degraded("meta", "missing")]);
    Ok(())
}

#[test]
fn a_role_completed_on_one_shard_and_degraded_on_another_is_degraded_once()
-> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.completed_roles = vec![RequiredRole::Combined];
    inputs.degraded = vec![degraded("combined", "cap")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "combined: cap")]
    );
    assert_eq!(decision.degraded, [degraded("combined", "cap")]);
    Ok(())
}

#[test]
fn a_required_role_reported_degraded_is_not_also_missing() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.completed_roles = Vec::new();
    inputs.degraded = vec![degraded("combined", "turns")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.degraded, [degraded("combined", "turns")]);
    Ok(())
}

#[test]
fn a_failed_coordinator_fails_an_exec_surface_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = full_trust_path();
    inputs.pr.classes = vec![PrClass::ExecSurface];
    inputs.coordinator = SupportOutcome::Failed {
        reason: "timeout".to_owned(),
    };

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(
            ReasonCode::RequiredRoleDegraded,
            "coordinator: timeout"
        )]
    );
    assert_eq!(decision.degraded, [degraded("coordinator", "timeout")]);
    Ok(())
}

#[test]
fn an_unavailable_verifier_warns_llm_degraded_on_an_unclassed_pr() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.verifier = SupportOutcome::Failed {
        reason: "unavailable".to_owned(),
    };

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "verifier: unavailable")]
    );
    assert_eq!(decision.degraded, [degraded("verifier", "unavailable")]);
    Ok(())
}

#[test]
fn a_full_pr_whose_findings_no_coordinator_grouped_is_degraded() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.coordinator = SupportOutcome::NotNeeded;
    inputs.findings = vec![confirmed("00000000000000c1", Severity::Minor)];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "coordinator: missing")]
    );
    Ok(())
}

#[test]
fn a_full_pr_without_findings_needs_no_coordinator() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.coordinator = SupportOutcome::NotNeeded;
    inputs.verifier = SupportOutcome::NotNeeded;

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    assert_eq!(decision.degraded, []);
    Ok(())
}

#[test]
fn a_deferred_stage_reports_its_deferral_and_no_missing_role() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.llm = LlmStage::Deferred {
        reason: Deferral::BudgetDeferred,
    };
    inputs.completed_roles = Vec::new();
    inputs.coordinator = SupportOutcome::NotNeeded;
    inputs.verifier = SupportOutcome::NotNeeded;

    let decision = decided(&inputs)?;

    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::LlmDegraded, "budget-deferred")]
    );
    assert_eq!(decision.degraded, []);
    Ok(())
}

#[test]
fn degraded_roles_are_listed_sorted_and_once() -> Result<(), Box<dyn Error>> {
    let mut inputs = full_trust_path();
    inputs.pr.owner_approval = OwnerApproval::Approved;
    inputs.completed_roles = vec![RequiredRole::Bugs, RequiredRole::Design];
    inputs.verifier = SupportOutcome::Failed {
        reason: "cap".to_owned(),
    };
    inputs.degraded = vec![
        degraded("security", "timeout"),
        degraded("bugs", "cap"),
        degraded("security", "timeout"),
    ];

    let decision = decided(&inputs)?;

    let listed = [
        degraded("bugs", "cap"),
        degraded("meta", "missing"),
        degraded("security", "timeout"),
        degraded("verifier", "cap"),
    ];
    assert_eq!(decision.degraded, listed);
    assert_eq!(
        decision.reasons,
        listed.map(|role| reason(
            ReasonCode::RequiredRoleDegraded,
            &format!("{}: {}", role.role, role.reason)
        ))
    );
    Ok(())
}

#[test]
fn the_inputs_digest_changes_with_each_role_run() -> Result<(), Box<dyn Error>> {
    let base = decided(&clean_inputs())?.inputs_digest;
    let mut fewer_roles = clean_inputs();
    fewer_roles.completed_roles.pop();
    let mut no_coordinator = clean_inputs();
    no_coordinator.coordinator = SupportOutcome::NotNeeded;
    let mut failed_verifier = clean_inputs();
    failed_verifier.verifier = SupportOutcome::Failed {
        reason: "cap".to_owned(),
    };

    let digests = [fewer_roles, no_coordinator, failed_verifier]
        .iter()
        .map(|inputs| decided(inputs).map(|d| d.inputs_digest))
        .collect::<Result<Vec<_>, _>>()?;

    assert!(digests.iter().all(|d| *d != base), "{base} in {digests:?}");
    Ok(())
}

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
    for (row, case) in CASES.iter().enumerate() {
        let mut inputs = full_trust_path();
        inputs.pr.classes = case.classes.to_vec();
        inputs.policy.block_on_infra_for = case.block_on_infra_for.to_vec();
        inputs.policy.on_infra_failure = case.on_infra_failure;
        inputs.completed_roles = vec![RequiredRole::Security, RequiredRole::Design];
        inputs.degraded = vec![degraded("bugs", "cap")];

        let decision = decided(&inputs)?;

        let (conclusion, code) = if case.fails {
            (Conclusion::Failure, ReasonCode::RequiredRoleDegraded)
        } else {
            (Conclusion::Success, ReasonCode::LlmDegraded)
        };
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

/// A `full` PR of class trust path that the owner approved, every required role completed.
fn full_trust_path() -> DecisionInputs {
    let mut inputs = clean_inputs();
    inputs.pr.tier = Tier::Full;
    inputs.pr.classes = vec![PrClass::TrustPath];
    inputs.pr.owner_approval = OwnerApproval::Approved;
    inputs
}
