//! Properties of `decide` over generated inputs: determinism down to the byte, `pending` only
//! from a deferred LLM stage, no silent success when the stage did not run or completed without
//! its required roles, a decision that reads back as written, and a required gate no waiver can
//! clear failing unless an `all` override applies.

use std::collections::BTreeSet;

use assay_domain::{
    Authorship, CallFailure, Conclusion, Counts, DecisionInputs, Deferral, DegradedRole,
    Disposition, FindingDisposition, Fingerprint, GateConclusion, GateFinding, GateOutcome,
    GateReport, InfraPolicy, LlmPolicy, LlmStage, NotRunReason, Override, OverrideScope,
    OwnerApproval, Policy, PrClass, PullRequest, Readiness, RequiredRole, ReviewedFinding, Rubric,
    Severity, StepAttribution, SupportOutcome, Tier, VerifierVerdict, Waiver, WeighIn, decide,
};
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::select;

use crate::support::{clean_inputs, fp as fingerprint, rubric_v1};

/// Few distinct fingerprints, so waivers often name a generated finding and fingerprints collide
/// across gates and rules.
fn fp() -> impl Strategy<Value = Fingerprint> {
    select(
        [
            "0000000000000000",
            "0000000000000001",
            "0000000000000002",
            "0000000000000003",
        ]
        .map(fingerprint)
        .to_vec(),
    )
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

fn support_outcome() -> impl Strategy<Value = SupportOutcome> {
    prop_oneof![
        Just(SupportOutcome::NotNeeded),
        Just(SupportOutcome::Completed),
        select(CallFailure::ALL.to_vec()).prop_map(|reason| SupportOutcome::Failed { reason }),
    ]
}

/// The specialist roles that completed, and how the coordinator and the verifier ran.
fn role_runs() -> impl Strategy<Value = (BTreeSet<RequiredRole>, SupportOutcome, SupportOutcome)> {
    (
        proptest::sample::subsequence(RequiredRole::ALL.to_vec(), 0..=RequiredRole::ALL.len())
            .prop_map(BTreeSet::from_iter),
        support_outcome(),
        support_outcome(),
    )
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
        (role_runs(), degraded),
        commands(),
    )
        .prop_map(
            |(policy, pr, gates, weigh_in, llm, findings, dispositions, roles, commands)| {
                let ((completed_roles, coordinator, verifier), degraded) = roles;
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
                    completed_roles,
                    coordinator,
                    verifier,
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

/// Whether rule 8a may leave `inputs` pending, read from the raw inputs alone: the LLM stage was
/// deferred, or it did not run on a tier that has one (`full`, `lite`, `micro`) without a real
/// draft to excuse it (`no-llm-tier` contradicts the tier; `draft` needs a PR that is still a
/// draft).
fn may_be_pending(inputs: &DecisionInputs) -> bool {
    match inputs.llm {
        LlmStage::Deferred { .. } => true,
        LlmStage::NotRun { reason } => {
            matches!(inputs.pr.tier, Tier::Full | Tier::Lite | Tier::Micro)
                && (reason == NotRunReason::NoLlmTier || inputs.pr.readiness == Readiness::Ready)
        }
        LlmStage::Completed => false,
    }
}

/// Clean inputs except for the LLM stage and what rule 8 reads beside it, so rule 8 decides the
/// conclusion.
fn llm_inputs() -> impl Strategy<Value = DecisionInputs> {
    (
        select(Tier::ALL.to_vec()),
        select(Readiness::ALL.to_vec()),
        select(Authorship::ALL.to_vec()),
        0_u32..5,
        select(InfraPolicy::ALL.to_vec()),
        llm_stage(),
    )
        .prop_map(
            |(tier, readiness, authorship, requeues, on_infra_failure, llm)| {
                let mut inputs = clean_inputs();
                inputs.pr.tier = tier;
                inputs.pr.readiness = readiness;
                inputs.pr.authorship = authorship;
                inputs.pr.requeues = requeues;
                inputs.policy.on_infra_failure = on_infra_failure;
                inputs.llm = llm;
                inputs
            },
        )
}

/// Any inputs, made a ready PR on a tier with an LLM stage whose stage did not run.
fn ready_reviewed_not_run_inputs() -> impl Strategy<Value = DecisionInputs> {
    (
        prop_oneof![inputs(), llm_inputs()],
        select(vec![Tier::Full, Tier::Lite, Tier::Micro]),
        select(NotRunReason::ALL.to_vec()),
    )
        .prop_map(|(mut inputs, tier, reason)| {
            inputs.pr.tier = tier;
            inputs.pr.readiness = Readiness::Ready;
            inputs.llm = LlmStage::NotRun { reason };
            inputs
        })
}

/// Any inputs, made a PR on a tier with an LLM stage whose stage completed while no specialist
/// role did.
fn completed_without_roles_inputs() -> impl Strategy<Value = DecisionInputs> {
    (
        prop_oneof![inputs(), llm_inputs()],
        select(vec![Tier::Full, Tier::Lite, Tier::Micro]),
    )
        .prop_map(|(mut inputs, tier)| {
            inputs.pr.tier = tier;
            inputs.llm = LlmStage::Completed;
            inputs.completed_roles = BTreeSet::new();
            inputs
        })
}

proptest! {
    // The cases these properties are about (every gate cleared by waivers, a stage that did not
    // run on a tier without one) are a few percent of generated inputs or fewer; 4,096 cases
    // run in about a second and find them reliably.
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

    #[test]
    fn the_findings_a_waiver_stops_counting_are_exactly_those_reported_waived(
        inputs in gate_inputs(),
    ) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");

        let decision = decide(&rubric, &inputs);

        // `gate_inputs` has no reviewer findings, so every count is a gate finding's: those
        // whose fingerprint is reported waived drop out, and every other one still counts.
        let waived: BTreeSet<Fingerprint> = decision.waived.iter().map(|w| w.fp).collect();
        let mut expected = Counts::default();
        for finding in inputs.gates.iter().flat_map(|gate| &gate.findings) {
            if waived.contains(&finding.fp) {
                continue;
            }
            let count = match finding.severity {
                Severity::Blocker => &mut expected.blocker,
                Severity::Major => &mut expected.major,
                Severity::Minor => &mut expected.minor,
                Severity::Nit => &mut expected.nit,
            };
            *count += 1;
        }
        prop_assert_eq!(decision.counts, expected, "waived {:?}", waived);
    }

    #[test]
    fn pending_comes_only_from_a_deferred_or_unexcused_llm_stage(
        inputs in prop_oneof![inputs(), llm_inputs()],
    ) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");

        let decision = decide(&rubric, &inputs);

        prop_assert!(
            decision.conclusion != Conclusion::Pending || may_be_pending(&inputs),
            "{:?} from {:?} on {:?}", decision.reasons, inputs.llm, inputs.pr
        );
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
    fn a_ready_reviewed_pr_whose_llm_stage_did_not_run_never_succeeds_silently(
        inputs in ready_reviewed_not_run_inputs(),
    ) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");

        let decision = decide(&rubric, &inputs);

        prop_assert!(
            decision.conclusion != Conclusion::Success || !decision.reasons.is_empty(),
            "a reasonless success from {:?} on a ready {:?} PR", inputs.llm, inputs.pr.tier
        );
    }

    #[test]
    fn a_completed_stage_without_its_required_roles_never_succeeds_silently(
        inputs in completed_without_roles_inputs(),
    ) {
        let rubric = rubric_v1().expect("rubric/v1.toml parses");

        let decision = decide(&rubric, &inputs);

        prop_assert!(!decision.degraded.is_empty(), "nothing degraded on {:?}", inputs.pr);
        prop_assert!(
            decision.conclusion != Conclusion::Success || !decision.reasons.is_empty(),
            "a reasonless success on a {:?} PR", inputs.pr.tier
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
