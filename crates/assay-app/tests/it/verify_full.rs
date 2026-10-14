//! The blind verifier in `full`, and findings carried from earlier runs sharing its cap
//! (`docs/architecture/04-pipeline.md` §3, §4 step 8, §7), driven through a scripted runner.

use std::error::Error;

use assay_app::{
    ClaimRef, FindingRef, ModelOutcome, RateLimitObserved, VERIFIER_CAP, VerdictRefusal,
    VerifierFailure, VerifierStatus, VerifierTier, verify,
};
use assay_domain::{
    Aeu, CallFailure, Disposition, EventPayload, RateLimitStatus, Severity, SupportOutcome, Tier,
    VerifierVerdict,
};
use serde_json::{Value, json};

use crate::verify_fakes::{
    Scripted, bundle, carried, claims, finding, request, request_carrying, result, verdict,
};

/// `count` well-formed confirmations at one micro-AEU each.
fn confirming(count: usize) -> Scripted {
    Scripted::answering((0..count).map(|_| (verdict("confirmed", "seen"), 1)))
}

fn raised(document_index: usize) -> ClaimRef {
    ClaimRef::Raised(FindingRef::Finding {
        document: 0,
        index: document_index,
    })
}

#[test]
fn full_verifies_five_candidates_and_a_sixth_is_over_the_cap() -> Result<(), Box<dyn Error>> {
    let findings = (0..6)
        .map(|index| finding(index, Severity::Major))
        .collect::<Result<Vec<_>, _>>()?;
    let bundle = bundle()?;
    let runner = confirming(6);

    let verified = verify(&runner, request(VerifierTier::Full, &findings, &bundle));

    assert_eq!(runner.call_count(), 5);
    let statuses: Vec<_> = verified
        .findings
        .iter()
        .map(|found| (found.severity, found.verdict()))
        .collect();
    let confirmed = (Severity::Major, Some(VerifierVerdict::Confirmed));
    assert_eq!(
        statuses,
        [
            confirmed,
            confirmed,
            confirmed,
            confirmed,
            confirmed,
            (Severity::Major, None)
        ]
    );
    let last = verified.findings.last().ok_or("six findings out")?;
    assert_eq!(last.status, VerifierStatus::OverCap);
    assert_eq!(verified.outcome(), SupportOutcome::Completed);
    Ok(())
}

#[test]
fn the_full_tier_has_a_verifier() {
    assert_eq!(VerifierTier::try_from(Tier::Full), Ok(VerifierTier::Full));
}

#[test]
fn the_stage_budget_is_the_per_call_cap_times_the_calls() {
    let budgets = [VerifierTier::Full, VerifierTier::Lite, VerifierTier::Micro]
        .map(|tier| (tier.calls(), tier.budget()));

    assert_eq!(
        budgets,
        [
            (5, Aeu::from_micros(400_000)),
            (3, Aeu::from_micros(240_000)),
            (0, Aeu::ZERO)
        ]
    );
}

#[test]
fn carried_findings_go_first_by_severity_then_in_order() -> Result<(), Box<dyn Error>> {
    let carried = [
        carried(0, Severity::Minor)?,
        carried(1, Severity::Blocker)?,
        carried(2, Severity::Major)?,
        carried(3, Severity::Blocker)?,
    ];
    let findings = [finding(0, Severity::Major)?, finding(1, Severity::Blocker)?];
    let bundle = bundle()?;
    let runner = confirming(6);

    let verified = verify(
        &runner,
        request_carrying(VerifierTier::Full, &findings, &carried, &bundle),
    );

    assert_eq!(
        claims(&runner),
        [
            "carried 1",
            "carried 3",
            "carried 2",
            "carried 0",
            "unwrap on user input panics (1)"
        ]
    );
    let over: Vec<_> = verified
        .findings
        .iter()
        .chain(&verified.carried)
        .filter(|found| found.status == VerifierStatus::OverCap)
        .map(|found| found.at)
        .collect();
    assert_eq!(over, [raised(0)]);
    Ok(())
}

#[test]
fn carried_findings_at_lites_cap_leave_every_raised_candidate_over_it() -> Result<(), Box<dyn Error>>
{
    let carried = [
        carried(0, Severity::Major)?,
        carried(1, Severity::Major)?,
        carried(2, Severity::Major)?,
    ];
    let findings = [finding(0, Severity::Blocker)?];
    let bundle = bundle()?;
    let runner = confirming(4);

    let verified = verify(
        &runner,
        request_carrying(VerifierTier::Lite, &findings, &carried, &bundle),
    );

    assert_eq!(claims(&runner), ["carried 0", "carried 1", "carried 2"]);
    let [blocker] = verified.findings.as_slice() else {
        return Err("one finding in, one out".into());
    };
    assert_eq!(
        (blocker.severity, &blocker.status),
        (Severity::Blocker, &VerifierStatus::OverCap)
    );
    Ok(())
}

#[test]
fn a_carried_finding_past_the_cap_is_over_it() -> Result<(), Box<dyn Error>> {
    let carried = [
        carried(0, Severity::Major)?,
        carried(1, Severity::Major)?,
        carried(2, Severity::Major)?,
        carried(3, Severity::Nit)?,
    ];
    let bundle = bundle()?;
    let runner = confirming(4);

    let verified = verify(
        &runner,
        request_carrying(VerifierTier::Lite, &[], &carried, &bundle),
    );

    assert_eq!(runner.call_count(), 3);
    let statuses: Vec<_> = verified
        .carried
        .iter()
        .map(|found| (found.at, found.verdict()))
        .collect();
    let confirmed = Some(VerifierVerdict::Confirmed);
    assert_eq!(
        statuses,
        [
            (ClaimRef::Carried(0), confirmed),
            (ClaimRef::Carried(1), confirmed),
            (ClaimRef::Carried(2), confirmed),
            (ClaimRef::Carried(3), None),
        ]
    );
    let last = verified.carried.last().ok_or("four carried out")?;
    assert_eq!(
        (last.severity, &last.status),
        (Severity::Nit, &VerifierStatus::OverCap)
    );
    Ok(())
}

/// #212: a carried claim is asked 04 §7's question, so it gets the carried-verifier instruction.
#[test]
fn a_carried_claim_is_the_same_blind_five_fields_then_the_carried_question()
-> Result<(), Box<dyn Error>> {
    let carried = [carried(7, Severity::Minor)?];
    let bundle = bundle()?;
    let runner = confirming(1);

    verify(
        &runner,
        request_carrying(VerifierTier::Lite, &[], &carried, &bundle),
    );

    let calls = runner.calls.borrow();
    let [call] = calls.as_slice() else {
        return Err("one carried finding, one call".into());
    };
    let (claim, instruction) = call
        .user_prompt
        .as_str()
        .split_once("\n\n")
        .ok_or("the claim, a blank line, then the instruction")?;
    let claim: Value = serde_json::from_str(claim)?;
    assert_eq!(
        claim,
        json!({
            "file": "src/moved.rs",
            "line": 40,
            "class": "error-handling",
            "claim": "carried 7",
            "trigger": "an empty list reaches the index",
        })
    );
    assert_eq!(instruction, "VERIFIER-CARRIED");
    Ok(())
}

#[test]
fn a_raised_claim_in_a_run_with_carried_ones_keeps_the_verifier_instruction()
-> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Minor)?];
    let findings = [finding(0, Severity::Major)?];
    let bundle = bundle()?;
    let runner = confirming(2);

    verify(
        &runner,
        request_carrying(VerifierTier::Lite, &findings, &carried, &bundle),
    );

    let instructions: Vec<String> = runner
        .calls
        .borrow()
        .iter()
        .filter_map(|call| {
            call.user_prompt
                .as_str()
                .split_once("\n\n")
                .map(|(_, instruction)| instruction.to_owned())
        })
        .collect();
    assert_eq!(instructions, ["VERIFIER-CARRIED", "VERIFIER"]);
    Ok(())
}

#[test]
fn carried_verdicts_change_severity_and_come_first_in_the_events() -> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Blocker)?];
    let findings = [finding(0, Severity::Major)?];
    let bundle = bundle()?;
    let runner = Scripted::answering([
        (verdict("refuted", "the index is guarded"), 1),
        (verdict("confirmed", "seen"), 1),
    ]);

    let verified = verify(
        &runner,
        request_carrying(VerifierTier::Full, &findings, &carried, &bundle),
    );

    let [gone] = verified.carried.as_slice() else {
        return Err("one carried in, one out".into());
    };
    assert_eq!(
        (gone.at, gone.raised, gone.severity, gone.verdict()),
        (
            ClaimRef::Carried(0),
            Severity::Blocker,
            Severity::Minor,
            Some(VerifierVerdict::Refuted)
        )
    );
    assert_eq!(verified.outcome(), SupportOutcome::Completed);
    assert_eq!(
        verified.events().collect::<Vec<_>>(),
        [
            EventPayload::FindingVerified {
                verdict: VerifierVerdict::Refuted
            },
            EventPayload::FindingDisposed {
                code: Disposition::VerifierRejectedDowngraded,
                fp: None,
                from: Some(Severity::Blocker),
                to: Some(Severity::Minor),
            },
            EventPayload::FindingVerified {
                verdict: VerifierVerdict::Confirmed
            },
        ]
    );
    Ok(())
}

#[test]
fn micro_makes_no_call_for_carried_findings() -> Result<(), Box<dyn Error>> {
    let carried = [carried(0, Severity::Blocker)?];
    let bundle = bundle()?;
    let runner = Scripted::new([]);

    let verified = verify(
        &runner,
        request_carrying(VerifierTier::Micro, &[], &carried, &bundle),
    );

    assert_eq!(runner.call_count(), 0);
    let [kept] = verified.carried.as_slice() else {
        return Err("one carried in, one out".into());
    };
    assert_eq!(
        (kept.severity, &kept.status),
        (Severity::Blocker, &VerifierStatus::NoVerifier)
    );
    assert_eq!(verified.outcome(), SupportOutcome::NotNeeded);
    Ok(())
}

#[test]
fn a_failure_on_a_carried_finding_names_it_and_leaves_later_picks_unverified()
-> Result<(), Box<dyn Error>> {
    let carried = [
        carried(0, Severity::Blocker)?,
        carried(1, Severity::Blocker)?,
    ];
    let findings = [finding(0, Severity::Blocker)?];
    let bundle = bundle()?;
    let runner = Scripted::answering([
        (verdict("confirmed", "seen"), 1),
        (ModelOutcome::Timeout, 1),
    ]);

    let verified = verify(
        &runner,
        request_carrying(VerifierTier::Full, &findings, &carried, &bundle),
    );

    assert_eq!(
        verified.failure,
        Some(VerifierFailure::Call {
            at: ClaimRef::Carried(1),
            reason: CallFailure::Timeout
        })
    );
    let shown: Vec<_> = verified
        .carried
        .iter()
        .chain(&verified.findings)
        .map(|found| (found.severity, found.status == VerifierStatus::Unverified))
        .collect();
    assert_eq!(
        shown,
        [
            (Severity::Blocker, false),
            (Severity::Major, true),
            (Severity::Major, true)
        ]
    );
    Ok(())
}

#[test]
fn a_stage_failure_leaves_a_blocker_past_the_cap_at_its_severity() -> Result<(), Box<dyn Error>> {
    let findings = (0..6)
        .map(|index| finding(index, Severity::Blocker))
        .collect::<Result<Vec<_>, _>>()?;
    let bundle = bundle()?;
    let runner = Scripted::answering([(ModelOutcome::TurnsExhausted, 1)]);

    let verified = verify(&runner, request(VerifierTier::Full, &findings, &bundle));

    let shown: Vec<_> = verified
        .findings
        .iter()
        .map(|found| (found.severity, &found.status))
        .collect();
    let mut expected = vec![(Severity::Major, &VerifierStatus::Unverified); 5];
    expected.push((Severity::Blocker, &VerifierStatus::OverCap));
    assert_eq!(shown, expected);
    Ok(())
}

#[test]
fn an_attempt_gets_only_what_remains_of_the_stage_budget() -> Result<(), Box<dyn Error>> {
    let findings = (0..5)
        .map(|index| finding(index, Severity::Major))
        .collect::<Result<Vec<_>, _>>()?;
    let bundle = bundle()?;
    let runner = Scripted::answering([
        (ModelOutcome::Malformed, 80_000),
        (verdict("confirmed", "seen"), 80_000),
        (verdict("confirmed", "seen"), 80_000),
        (verdict("confirmed", "seen"), 80_000),
        (verdict("confirmed", "seen"), 50_000),
        (verdict("confirmed", "seen"), 30_000),
    ]);

    let verified = verify(&runner, request(VerifierTier::Full, &findings, &bundle));

    let budgets: Vec<u64> = runner
        .calls
        .borrow()
        .iter()
        .map(|call| call.budget.micros())
        .collect();
    assert_eq!(
        budgets,
        [80_000, 80_000, 80_000, 80_000, 80_000, 30_000],
        "the last candidate gets the $0.03 left of $0.40"
    );
    assert_eq!(
        (verified.outcome(), verified.cost),
        (SupportOutcome::Completed, Aeu::from_micros(400_000))
    );
    Ok(())
}

#[test]
fn a_spent_stage_budget_stops_the_stage_as_cap_without_a_call() -> Result<(), Box<dyn Error>> {
    let findings = [finding(0, Severity::Major)?, finding(1, Severity::Major)?];
    let bundle = bundle()?;
    let runner = Scripted::answering([
        (verdict("confirmed", "seen"), 240_000),
        (verdict("confirmed", "never asked"), 1),
    ]);

    let verified = verify(&runner, request(VerifierTier::Lite, &findings, &bundle));

    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        verified.failure,
        Some(VerifierFailure::Call {
            at: raised(1),
            reason: CallFailure::Cap
        })
    );
    let second = verified.findings.get(1).ok_or("two findings out")?;
    assert_eq!(
        (second.severity, &second.status),
        (Severity::Major, &VerifierStatus::Unverified)
    );
    assert_eq!(verified.attempts, 1);
    Ok(())
}

/// #212 F2: the budget runs out between a refused first attempt and its retry.
#[test]
fn a_refused_first_attempt_that_spends_the_budget_stops_before_its_retry_as_cap()
-> Result<(), Box<dyn Error>> {
    let findings = [finding(0, Severity::Major)?];
    let bundle = bundle()?;
    let runner = Scripted::answering([
        (verdict("maybe", "a code outside the three"), 240_000),
        (verdict("confirmed", "never asked"), 1),
    ]);

    let verified = verify(&runner, request(VerifierTier::Lite, &findings, &bundle));

    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        verified.failure,
        Some(VerifierFailure::Call {
            at: raised(0),
            reason: CallFailure::Cap
        })
    );
    assert_eq!(verified.attempts, 1);
    assert_eq!(verified.refusals, [VerdictRefusal::Verdict]);
    Ok(())
}

#[test]
fn each_refused_verdict_is_kept_in_order() -> Result<(), Box<dyn Error>> {
    let findings = [finding(0, Severity::Major)?, finding(1, Severity::Major)?];
    let bundle = bundle()?;
    let too_long = "a".repeat(201);
    let runner = Scripted::answering([
        (verdict("maybe", "seen"), 1),
        (verdict("confirmed", "seen"), 1),
        (verdict("refuted", &too_long), 1),
        (ModelOutcome::Malformed, 1),
    ]);

    let verified = verify(&runner, request(VerifierTier::Lite, &findings, &bundle));

    assert_eq!(
        verified.refusals,
        [VerdictRefusal::Verdict, VerdictRefusal::TooLong]
    );
    assert_eq!(
        verified.outcome(),
        SupportOutcome::Failed {
            reason: CallFailure::Malformed
        }
    );
    Ok(())
}

#[test]
fn the_rate_limits_of_every_attempt_are_kept_in_order() -> Result<(), Box<dyn Error>> {
    let findings = [finding(0, Severity::Major)?, finding(1, Severity::Major)?];
    let bundle = bundle()?;
    let limited = |outcome, status, resets_at| {
        let mut answer = result(outcome, 1);
        answer.rate_limits = vec![RateLimitObserved { status, resets_at }];
        Ok(answer)
    };
    let runner = Scripted::new([
        limited(
            ModelOutcome::Malformed,
            RateLimitStatus::AllowedWarning,
            None,
        ),
        limited(
            verdict("confirmed", "seen"),
            RateLimitStatus::AllowedWarning,
            Some(10),
        ),
        limited(
            verdict("confirmed", "seen"),
            RateLimitStatus::Rejected,
            Some(20),
        ),
    ]);

    let verified = verify(&runner, request(VerifierTier::Lite, &findings, &bundle));

    let observed: Vec<_> = verified
        .rate_limits
        .iter()
        .map(|limit| (limit.status, limit.resets_at))
        .collect();
    assert_eq!(
        observed,
        [
            (RateLimitStatus::AllowedWarning, None),
            (RateLimitStatus::AllowedWarning, Some(10)),
            (RateLimitStatus::Rejected, Some(20)),
        ]
    );
    assert_eq!(verified.cost, Aeu::from_micros(3));
    Ok(())
}

#[test]
fn a_runner_error_is_charged_at_the_attempts_budget() -> Result<(), Box<dyn Error>> {
    let findings = [finding(0, Severity::Major)?, finding(1, Severity::Major)?];
    let bundle = bundle()?;
    let runner = Scripted::new([Ok(result(verdict("confirmed", "seen"), 200_000))]);

    let verified = verify(&runner, request(VerifierTier::Lite, &findings, &bundle));

    assert_eq!(
        (verified.attempts, verified.cost),
        (2, Aeu::from_micros(240_000)),
        "the second call had $0.04 left and is charged that, not {VERIFIER_CAP}"
    );
    Ok(())
}
