//! `decide` waive and override (`docs/architecture/04-pipeline.md` §5), and the parts of the
//! decision that describe findings: counts, dispositions and the digests.

use std::error::Error;

use assay_domain::{
    Conclusion, Counts, Decision, DecisionInputs, Deferral, DegradedRole, Disposition,
    FindingDisposition, InfraPolicy, LlmStage, Override, OverrideScope, OwnerApproval, PrClass,
    ReasonCode, Severity, Tier, Waived, Waiver, decide,
};

use crate::support::{attested_gate, clean_inputs, confirmed, fp, gate_finding, reason, rubric_v1};

/// `printf 'gate-failed\n' | sha256sum`
const GATE_FAILED_DIGEST: &str =
    "sha256:edfab635b96543711e1a2662a111f30252334d5a72e82c9d11bb62e35ad7e231";
/// `printf 'budget-deferred\n' | sha256sum`
const BUDGET_DEFERRED_DIGEST: &str =
    "sha256:ab60963112e40bec2b8fcd6c5c6970d3a93d64968fce1754d512ab6b8e767e1d";
/// `printf 'gate-failed\nneeds-split\n' | sha256sum`
const GATE_FAILED_NEEDS_SPLIT_DIGEST: &str =
    "sha256:f9838b74a72d134a2d6270babd99be12097ea49c033b6ab2e780ef72f630024f";

/// `printf 'base-not-protected\n' | sha256sum`
const BASE_NOT_PROTECTED_DIGEST: &str =
    "sha256:d1fa2bd6cdcd5ce1e62470b52b22ceac7266c7cac035505c75ad7cf54b06962c";

/// `printf 'required-role-degraded\n' | sha256sum`
const REQUIRED_ROLE_DEGRADED_DIGEST: &str =
    "sha256:aa332cda0bc178fd07e089e308e9526626762da10c53c4cf1e6c89bca1282473";
/// `printf 'infra-llm-unavailable\n' | sha256sum`
const INFRA_LLM_UNAVAILABLE_DIGEST: &str =
    "sha256:2179015d990775098f5b42c58d32f51aeeb8d0a5de890b12e19c8d32b840f457";

fn decided(inputs: &DecisionInputs) -> Result<Decision, Box<dyn Error>> {
    Ok(decide(&rubric_v1()?, inputs))
}

/// A failed temper gate, its steps attested, with a raised baseline (waivable) and, when
/// `with_clippy`, a clippy finding (never waivable).
fn temper_failed(with_clippy: bool) -> DecisionInputs {
    let mut findings = vec![gate_finding("00000000000000a1", "raised", Severity::Major)];
    if with_clippy {
        findings.push(gate_finding(
            "00000000000000a2",
            "clippy::unwrap_used",
            Severity::Blocker,
        ));
    }
    let mut inputs = clean_inputs();
    inputs.gates = vec![attested_gate("temper", findings)];
    inputs
}

fn waiver(fp_text: &str) -> Waiver {
    Waiver {
        fp: fp(fp_text),
        by: "owner".to_owned(),
    }
}

fn override_record(scope: OverrideScope, digest: &str) -> Override {
    Override {
        scope,
        reasons_digest: digest.to_owned(),
        by: "owner".to_owned(),
    }
}

#[test]
fn waiving_the_only_human_decision_finding_clears_the_gate() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(false);
    inputs.waivers = vec![waiver("00000000000000a1")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    assert_eq!(
        decision.waived,
        [Waived {
            fp: fp("00000000000000a1"),
            by: "owner".to_owned(),
        }]
    );
    assert_eq!(
        decision.dispositions,
        [FindingDisposition {
            fp: fp("00000000000000a1"),
            code: Disposition::WaivedByOwner,
        }]
    );
    Ok(())
}

#[test]
fn a_waiver_never_removes_a_clippy_finding() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(true);
    inputs.waivers = vec![waiver("00000000000000a2")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    assert_eq!(decision.waived, []);
    Ok(())
}

#[test]
fn waiving_one_finding_leaves_the_gate_failed_by_the_others() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(true);
    inputs.waivers = vec![waiver("00000000000000a1")];

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    assert_eq!(
        decision.waived.len(),
        1,
        "the raised finding is still waived"
    );
    Ok(())
}

#[test]
fn an_all_override_neutralises_a_gate_failure_and_keeps_its_reasons() -> Result<(), Box<dyn Error>>
{
    let mut inputs = temper_failed(true);
    inputs.override_record = Some(override_record(OverrideScope::All, GATE_FAILED_DIGEST));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Neutral);
    assert_eq!(
        decision.reasons,
        [
            reason(ReasonCode::GateFailed, "temper"),
            reason(ReasonCode::Override, "all"),
        ]
    );
    assert_eq!(decision.reasons_digest(), GATE_FAILED_DIGEST);
    Ok(())
}

#[test]
fn an_llm_override_does_not_neutralise_a_gate_failure() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(true);
    inputs.override_record = Some(override_record(OverrideScope::Llm, GATE_FAILED_DIGEST));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    Ok(())
}

#[test]
fn an_llm_override_neutralises_a_pending_deferral() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.classes = vec![PrClass::TrustPath];
    inputs.pr.owner_approval = OwnerApproval::Approved;
    inputs.llm = LlmStage::Deferred {
        reason: Deferral::BudgetDeferred,
    };
    inputs.override_record = Some(override_record(OverrideScope::Llm, BUDGET_DEFERRED_DIGEST));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Neutral);
    assert_eq!(
        decision.reasons.iter().map(|r| r.code).collect::<Vec<_>>(),
        [ReasonCode::BudgetDeferred, ReasonCode::Override]
    );
    Ok(())
}

#[test]
fn an_override_is_void_once_a_reason_is_added() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(true);
    inputs.pr.tier = Tier::NeedsSplit;
    inputs.override_record = Some(override_record(OverrideScope::All, GATE_FAILED_DIGEST));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons_digest(), GATE_FAILED_NEEDS_SPLIT_DIGEST);
    assert!(
        decision
            .reasons
            .iter()
            .all(|r| r.code != ReasonCode::Override),
        "{:?}",
        decision.reasons
    );
    Ok(())
}

#[test]
fn an_llm_override_neutralises_a_degraded_required_role() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.policy.on_infra_failure = InfraPolicy::Block;
    inputs.degraded = vec![DegradedRole {
        role: "bugs".to_owned(),
        reason: "cap".to_owned(),
    }];
    inputs.override_record = Some(override_record(
        OverrideScope::Llm,
        REQUIRED_ROLE_DEGRADED_DIGEST,
    ));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Neutral);
    assert_eq!(
        decision.reasons,
        [
            reason(ReasonCode::RequiredRoleDegraded, "bugs: cap"),
            reason(ReasonCode::Override, "llm"),
        ]
    );
    Ok(())
}

#[test]
fn an_llm_override_neutralises_a_deferral_that_ran_out_of_requeues() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.policy.on_infra_failure = InfraPolicy::Block;
    inputs.pr.requeues = 3;
    inputs.llm = LlmStage::Deferred {
        reason: Deferral::InfraLlmUnavailable,
    };
    inputs.override_record = Some(override_record(
        OverrideScope::Llm,
        INFRA_LLM_UNAVAILABLE_DIGEST,
    ));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Neutral);
    assert_eq!(
        decision.reasons.iter().map(|r| r.code).collect::<Vec<_>>(),
        [ReasonCode::InfraLlmUnavailable, ReasonCode::Override]
    );
    Ok(())
}

/// The override names reasons the PR no longer has (wi-8aabfb): it is void, and the remaining
/// reasons decide.
#[test]
fn an_override_is_void_once_a_quoted_reason_disappears() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(true);
    inputs.override_record = Some(override_record(
        OverrideScope::All,
        GATE_FAILED_NEEDS_SPLIT_DIGEST,
    ));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    Ok(())
}

#[test]
fn an_override_without_an_actor_is_not_applied() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(true);
    let mut record = override_record(OverrideScope::All, GATE_FAILED_DIGEST);
    record.by = String::new();
    inputs.override_record = Some(record);

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(decision.reasons, [reason(ReasonCode::GateFailed, "temper")]);
    Ok(())
}

/// R3-03: the check is not required on an unprotected base, so a `neutral` there would only pass
/// a stale check if the PR is retargeted; an override never applies to it.
#[test]
fn an_all_override_never_neutralises_an_unprotected_base() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.pr.base_ref = "scratch".to_owned();
    inputs.override_record = Some(override_record(
        OverrideScope::All,
        BASE_NOT_PROTECTED_DIGEST,
    ));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        decision.reasons,
        [reason(ReasonCode::BaseNotProtected, "scratch")]
    );
    assert_eq!(decision.reasons_digest(), BASE_NOT_PROTECTED_DIGEST);
    Ok(())
}

#[test]
fn an_override_leaves_a_success_alone() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    inputs.override_record = Some(override_record(
        OverrideScope::All,
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ));

    let decision = decided(&inputs)?;

    assert_eq!(decision.conclusion, Conclusion::Success);
    assert_eq!(decision.reasons, []);
    Ok(())
}

#[test]
fn counts_hold_reviewer_findings_and_unwaived_gate_findings() -> Result<(), Box<dyn Error>> {
    let mut inputs = temper_failed(true);
    inputs.waivers = vec![waiver("00000000000000a1")];
    inputs.findings = vec![
        confirmed("0000000000000001", Severity::Major),
        confirmed("0000000000000002", Severity::Minor),
        confirmed("0000000000000003", Severity::Nit),
    ];

    let decision = decided(&inputs)?;

    assert_eq!(
        decision.counts,
        Counts {
            blocker: 1,
            major: 1,
            minor: 1,
            nit: 1,
        }
    );
    Ok(())
}

#[test]
fn upstream_dispositions_are_kept_sorted() -> Result<(), Box<dyn Error>> {
    let mut inputs = clean_inputs();
    let disposition = |fp_text: &str, code| FindingDisposition {
        fp: fp(fp_text),
        code,
    };
    inputs.dispositions = vec![
        disposition("00000000000000f2", Disposition::NoTrigger),
        disposition("00000000000000f1", Disposition::DuplicateOfGate),
    ];

    let decision = decided(&inputs)?;

    assert_eq!(
        decision.dispositions,
        [
            disposition("00000000000000f1", Disposition::DuplicateOfGate),
            disposition("00000000000000f2", Disposition::NoTrigger),
        ]
    );
    Ok(())
}

#[test]
fn the_inputs_digest_is_a_sha256_in_lowercase_hex() -> Result<(), Box<dyn Error>> {
    let decision = decided(&clean_inputs())?;

    let hex = decision
        .inputs_digest
        .strip_prefix("sha256:")
        .ok_or("no sha256: prefix")?;

    assert_eq!(hex.len(), 64, "{hex}");
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{hex}"
    );
    Ok(())
}

#[test]
fn the_inputs_digest_changes_with_any_input() -> Result<(), Box<dyn Error>> {
    let base = decided(&clean_inputs())?.inputs_digest;
    let mut renamed = clean_inputs();
    renamed.decision_key.push('x');
    let mut requeued = clean_inputs();
    requeued.pr.requeues = 1;
    let mut waived = clean_inputs();
    waived.waivers = vec![waiver("00000000000000a1")];

    let digests = [renamed, requeued, waived]
        .iter()
        .map(|inputs| decided(inputs).map(|d| d.inputs_digest))
        .collect::<Result<Vec<_>, _>>()?;

    assert!(digests.iter().all(|d| *d != base), "{base} in {digests:?}");
    Ok(())
}
