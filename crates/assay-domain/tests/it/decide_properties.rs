//! Properties of `decide` over generated inputs: determinism down to the byte, `pending` only
//! from a deferred LLM stage, and a decision that reads back as written.

use assay_domain::{
    Authorship, Conclusion, DecisionInputs, Deferral, DegradedRole, Disposition,
    FindingDisposition, GateConclusion, GateFinding, GateOutcome, GateReport, InfraPolicy,
    LlmPolicy, LlmStage, NotRunReason, Override, OverrideScope, OwnerApproval, Policy, PrClass,
    PullRequest, ReviewedFinding, Severity, Tier, VerifierVerdict, Waiver, WeighIn, decide,
};
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::select;

use crate::support::rubric_v1;

fn fp() -> impl Strategy<Value = String> {
    "[0-9a-f]{2}".prop_map(|tail| format!("00000000000000{tail}"))
}

fn gate_report() -> impl Strategy<Value = GateReport> {
    let outcome = prop_oneof![
        (
            select(GateConclusion::ALL.to_vec()),
            select(vec![Vec::new(), vec!["fmt".to_owned()]]),
        )
            .prop_map(|(conclusion, failed_steps)| GateOutcome::Ran {
                conclusion,
                failed_steps,
            }),
        "[a-z]{1,8}".prop_map(|detail| GateOutcome::Error { detail }),
        Just(GateOutcome::Skipped),
    ];
    let finding = (
        fp(),
        option::of(select(vec![
            "raised",
            "exception",
            "clippy::unwrap_used",
            "build",
        ])),
        select(Severity::ALL.to_vec()),
    )
        .prop_map(|(fp, rule, severity)| GateFinding {
            fp,
            rule: rule.map(str::to_owned),
            severity,
        });
    (
        select(vec!["temper", "custom-lint"]),
        any::<bool>(),
        outcome,
        vec(finding, 0..4),
    )
        .prop_map(|(name, required, outcome, findings)| GateReport {
            name: name.to_owned(),
            required,
            outcome,
            findings,
        })
}

fn reviewed_finding() -> impl Strategy<Value = ReviewedFinding> {
    (
        fp(),
        "g[0-3]",
        select(vec!["bugs", "combined", "combined-micro"]),
        select(Severity::ALL.to_vec()),
        any::<bool>(),
        option::of(select(VerifierVerdict::ALL.to_vec())),
    )
        .prop_map(
            |(fp, group_id, role, severity, pre_existing, verdict)| ReviewedFinding {
                fp,
                group_id,
                role: role.to_owned(),
                severity,
                pre_existing,
                verdict,
            },
        )
}

fn policy() -> impl Strategy<Value = Policy> {
    (
        select(vec![vec!["main".to_owned()], Vec::new()]),
        select(LlmPolicy::ALL.to_vec()),
        select(LlmPolicy::ALL.to_vec()),
        select(InfraPolicy::ALL.to_vec()),
        proptest::sample::subsequence(PrClass::ALL.to_vec(), 0..=2),
    )
        .prop_map(
            |(protected_bases, llm_blockers, llm_majors, on_infra_failure, block_on_infra_for)| {
                Policy {
                    protected_bases,
                    llm_blockers,
                    llm_majors,
                    on_infra_failure,
                    block_on_infra_for,
                }
            },
        )
}

fn pull_request() -> impl Strategy<Value = PullRequest> {
    (
        0_u32..3,
        select(Tier::ALL.to_vec()),
        proptest::sample::subsequence(PrClass::ALL.to_vec(), 0..=2),
        select(OwnerApproval::ALL.to_vec()),
        select(Authorship::ALL.to_vec()),
        0_u32..5,
    )
        .prop_map(
            |(open_prs_on_head, tier, classes, owner_approval, authorship, requeues)| PullRequest {
                base_ref: "main".to_owned(),
                open_prs_on_head,
                tier,
                classes,
                owner_approval,
                authorship,
                requeues,
            },
        )
}

fn llm_stage() -> impl Strategy<Value = LlmStage> {
    prop_oneof![
        select(NotRunReason::ALL.to_vec()).prop_map(|reason| LlmStage::NotRun { reason }),
        Just(LlmStage::Completed),
        select(Deferral::ALL.to_vec()).prop_map(|reason| LlmStage::Deferred { reason }),
    ]
}

fn commands() -> impl Strategy<Value = (Vec<Waiver>, Option<Override>)> {
    let waiver = fp().prop_map(|fp| Waiver {
        fp,
        by: "owner".to_owned(),
    });
    let record =
        (select(OverrideScope::ALL.to_vec()), "[0-9a-f]{4}").prop_map(|(scope, digest)| Override {
            scope,
            reasons_digest: format!("sha256:{digest}"),
            by: "owner".to_owned(),
        });
    (vec(waiver, 0..3), option::of(record))
}

fn inputs() -> impl Strategy<Value = DecisionInputs> {
    let dispositions = vec(
        (fp(), select(Disposition::ALL.to_vec()))
            .prop_map(|(fp, code)| FindingDisposition { fp, code }),
        0..3,
    );
    let degraded = vec(
        (
            select(vec!["bugs", "security"]),
            select(vec!["cap", "timeout"]),
        )
            .prop_map(|(role, reason)| DegradedRole {
                role: role.to_owned(),
                reason: reason.to_owned(),
            }),
        0..2,
    );
    (
        policy(),
        pull_request(),
        vec(gate_report(), 0..3),
        select(WeighIn::ALL.to_vec()),
        llm_stage(),
        vec(reviewed_finding(), 0..6),
        dispositions,
        degraded,
        commands(),
    )
        .prop_map(
            |(policy, pr, gates, weigh_in, llm, findings, dispositions, degraded, commands)| {
                let (waivers, override_record) = commands;
                DecisionInputs {
                    decision_key: "dec-42-1".to_owned(),
                    evidence_key: "run-42-1".to_owned(),
                    policy,
                    pr,
                    gates,
                    weigh_in,
                    llm,
                    findings,
                    dispositions,
                    degraded,
                    waivers,
                    override_record,
                }
            },
        )
}

proptest! {
    #[test]
    fn identical_inputs_give_byte_identical_decisions(inputs in inputs()) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");
        // A separate but equal value: the property is about equal inputs, not one allocation.
        let copy = inputs.clone();

        let first = serde_json::to_vec(&decide(&rubric, &inputs)).expect("a decision serialises");
        let second = serde_json::to_vec(&decide(&rubric, &copy)).expect("a decision serialises");

        prop_assert_eq!(first, second);
    }

    #[test]
    fn pending_comes_only_from_a_deferred_llm_stage(inputs in inputs()) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");

        let decision = decide(&rubric, &inputs);

        prop_assert!(
            decision.conclusion != Conclusion::Pending
                || matches!(inputs.llm.effective(inputs.pr.tier), LlmStage::Deferred { .. }),
            "{:?} from {:?}", decision.reasons, inputs.llm
        );
    }

    #[test]
    fn a_decision_reads_back_as_written(inputs in inputs()) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");

        let decision = decide(&rubric, &inputs);

        let written = serde_json::to_vec(&decision).expect("a decision serialises");
        let read: assay_domain::Decision =
            serde_json::from_slice(&written).expect("a written decision reads back");

        prop_assert_eq!(read, decision);
    }
}
