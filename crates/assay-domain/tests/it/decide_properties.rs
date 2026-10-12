//! Properties of `decide` over generated inputs: determinism down to the byte, `pending` only
//! from a deferred LLM stage, a decision that reads back as written, and a required gate no
//! waiver can clear failing unless an `all` override applies.

use assay_domain::{
    Authorship, Conclusion, DecisionInputs, Deferral, DegradedRole, Disposition,
    FindingDisposition, GateConclusion, GateFinding, GateOutcome, GateReport, InfraPolicy,
    LlmPolicy, LlmStage, NotRunReason, Override, OverrideScope, OwnerApproval, Policy, PrClass,
    PullRequest, Readiness, ReviewedFinding, Rubric, Severity, StepAttribution, Tier,
    VerifierVerdict, Waiver, WeighIn, decide,
};
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::select;

use crate::support::{clean_inputs, rubric_v1};

/// Few distinct fingerprints, so waivers often name a generated finding and fingerprints collide
/// across gates and rules.
fn fp() -> impl Strategy<Value = String> {
    "[0-3]".prop_map(|tail| format!("000000000000000{tail}"))
}

/// Whether no waiver can clear `gate`'s rule 1 failure: it errored or was skipped, or it failed
/// with unattested or failed steps, with no findings, or with a finding whose rule is not
/// waivable for this gate.
fn unclearable(rubric: &Rubric, gate: &GateReport) -> bool {
    if !gate.required {
        return false;
    }
    match &gate.outcome {
        GateOutcome::Error { .. } | GateOutcome::Skipped => true,
        GateOutcome::Ran {
            conclusion: GateConclusion::Fail,
            steps,
        } => {
            !matches!(steps, StepAttribution::Known(failed) if failed.is_empty())
                || gate.findings.is_empty()
                || gate.findings.iter().any(|finding| {
                    !finding
                        .rule
                        .as_deref()
                        .is_some_and(|rule| rubric.is_waivable(&gate.name, rule))
                })
        }
        GateOutcome::Ran { .. } => false,
    }
}

fn gate_report() -> impl Strategy<Value = GateReport> {
    let outcome = prop_oneof![
        (
            select(GateConclusion::ALL.to_vec()),
            select(vec![
                StepAttribution::Unknown,
                StepAttribution::Known(Vec::new()),
                StepAttribution::Known(vec!["fmt".to_owned()]),
            ]),
        )
            .prop_map(|(conclusion, steps)| GateOutcome::Ran { conclusion, steps }),
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
        select(Readiness::ALL.to_vec()),
        0_u32..5,
    )
        .prop_map(
            |(open_prs_on_head, tier, classes, owner_approval, authorship, readiness, requeues)| {
                PullRequest {
                    base_ref: "main".to_owned(),
                    open_prs_on_head,
                    tier,
                    classes,
                    owner_approval,
                    authorship,
                    readiness,
                    requeues,
                }
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

/// Clean inputs except for generated gates and waivers, so rule 1 is the only rule that can
/// fail and waivers often matter.
fn gate_inputs() -> impl Strategy<Value = DecisionInputs> {
    let finding = (
        fp(),
        select(vec!["raised", "exception", "clippy::unwrap_used"]),
    )
        .prop_map(|(fp, rule)| GateFinding {
            fp,
            rule: Some(rule.to_owned()),
            severity: Severity::Major,
        });
    let steps = select(vec![
        StepAttribution::Unknown,
        StepAttribution::Known(Vec::new()),
        StepAttribution::Known(vec!["fmt".to_owned()]),
    ]);
    let failed_gate = (
        select(vec!["temper", "custom-lint"]),
        steps,
        vec(finding, 0..3),
    )
        .prop_map(|(name, steps, findings)| GateReport {
            name: name.to_owned(),
            required: true,
            outcome: GateOutcome::Ran {
                conclusion: GateConclusion::Fail,
                steps,
            },
            findings,
        });
    let gate = prop_oneof![3 => failed_gate, 1 => gate_report()];
    let waiver = fp().prop_map(|fp| Waiver {
        fp,
        by: "owner".to_owned(),
    });
    (vec(gate, 1..3), vec(waiver, 0..5)).prop_map(|(gates, waivers)| {
        let mut inputs = clean_inputs();
        inputs.gates = gates;
        inputs.waivers = waivers;
        inputs
    })
}

proptest! {
    // The waiver-clears-everything case is a few percent of generated inputs; 4,096 cases run in
    // well under a second and find a gate wrongly cleared reliably.
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn a_gate_no_waiver_can_clear_fails_unless_an_all_override_applies(
        inputs in prop_oneof![inputs(), gate_inputs()],
    ) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");
        if !inputs.gates.iter().any(|gate| unclearable(&rubric, gate)) {
            // The property says nothing about inputs whose every gate could be cleared.
            return Ok(());
        }

        let decision = decide(&rubric, &inputs);

        let all_override = inputs
            .override_record
            .as_ref()
            .is_some_and(|record| record.scope == OverrideScope::All);
        prop_assert!(
            decision.conclusion == Conclusion::Failure
                || (decision.conclusion == Conclusion::Neutral && all_override),
            "{:?} with {:?}", decision.conclusion, decision.reasons
        );

        // The same inputs under a valid override of each scope: only `all` may neutralise.
        for scope in OverrideScope::ALL {
            let mut overridden = inputs.clone();
            overridden.override_record = Some(Override {
                scope,
                reasons_digest: decision.reasons_digest(),
                by: "owner".to_owned(),
            });

            let conclusion = decide(&rubric, &overridden).conclusion;

            prop_assert!(
                conclusion == Conclusion::Failure
                    || (conclusion == Conclusion::Neutral && scope == OverrideScope::All),
                "{conclusion:?} under {scope:?}"
            );
        }
    }

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
                || matches!(inputs.llm.effective(&inputs.pr), LlmStage::Deferred { .. }),
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
