//! What becomes of each carried finding after the verifier (`docs/architecture/04-pipeline.md`
//! §6, §7, #99): refuted resolves its thread with a note, confirmed and unclear stay open,
//! one past the cap is "not re-verified (cap)" with no verdict (so rules 6-7 skip it), and
//! `micro` re-checks the anchor only.

use std::collections::BTreeMap;
use std::error::Error;

use assay_app::{
    CarriedOutcome, CarriedState, Change, ModelOutcome, NotReverified, Verification, VerifierTier,
    recheck_anchors, settle_carried, verify,
};
use assay_domain::{Severity, VerifierVerdict};

use crate::verify_fakes::{Scripted, Unreachable, bundle, carried, request_carrying, verdict};

/// `(severity, verdict, state)` of each outcome.
fn shown(outcomes: &[CarriedOutcome]) -> Vec<(Severity, Option<VerifierVerdict>, CarriedState)> {
    outcomes
        .iter()
        .map(|outcome| (outcome.severity, outcome.verdict, outcome.state.clone()))
        .collect()
}

/// Verifies `carried` in `tier` with `runner`, then settles them.
fn settled(
    tier: VerifierTier,
    count: usize,
    runner: &Scripted,
) -> Result<(Vec<CarriedOutcome>, Verification<Unreachable>), Box<dyn Error>> {
    let carried = (0..count)
        .map(|index| carried(index, Severity::Blocker))
        .collect::<Result<Vec<_>, _>>()?;
    let bundle = bundle()?;
    let verification = verify(runner, request_carrying(tier, &[], &carried, &bundle));
    Ok((settle_carried(&carried, &verification), verification))
}

#[test]
fn a_refuted_carried_finding_is_resolved_with_an_inert_note_and_kept_at_minor()
-> Result<(), Box<dyn Error>> {
    let runner = Scripted::answering([(verdict("refuted", "fixed in parse_input"), 1)]);

    let (outcomes, _) = settled(VerifierTier::Lite, 1, &runner)?;

    assert_eq!(
        shown(&outcomes),
        [(
            Severity::Minor,
            Some(VerifierVerdict::Refuted),
            CarriedState::Resolved {
                note: "Resolved by assay: re-verified at this head, the defect is no longer \
                       present.\n\nVerifier: fixed in parse\\_input"
                    .to_owned()
            }
        )]
    );
    Ok(())
}

#[test]
fn confirmed_and_unclear_carried_findings_stay_open() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::answering([
        (verdict("confirmed", "still unguarded"), 1),
        (verdict("unclear", "ran out of turns"), 1),
    ]);

    let (outcomes, _) = settled(VerifierTier::Lite, 2, &runner)?;

    assert_eq!(
        shown(&outcomes),
        [
            (
                Severity::Blocker,
                Some(VerifierVerdict::Confirmed),
                CarriedState::Open
            ),
            (
                Severity::Major,
                Some(VerifierVerdict::Unclear),
                CarriedState::Open
            ),
        ]
    );
    Ok(())
}

#[test]
fn a_carried_finding_past_the_cap_is_not_re_verified_and_has_no_verdict()
-> Result<(), Box<dyn Error>> {
    let runner = Scripted::answering((0..3).map(|_| (verdict("confirmed", "seen"), 1)));

    let (outcomes, _) = settled(VerifierTier::Lite, 4, &runner)?;

    let [.., last] = outcomes.as_slice() else {
        return Err("four carried in, four out".into());
    };
    assert_eq!(
        (last.severity, last.verdict, &last.state),
        (
            Severity::Blocker,
            None,
            &CarriedState::NotReverified(NotReverified::Cap)
        )
    );
    assert_eq!(NotReverified::Cap.label(), "not re-verified (cap)");
    Ok(())
}

#[test]
fn a_verifier_failure_leaves_the_carried_finding_open_unverified() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::answering([(ModelOutcome::Timeout, 1)]);

    let (outcomes, _) = settled(VerifierTier::Lite, 1, &runner)?;

    assert_eq!(
        shown(&outcomes),
        [(
            Severity::Major,
            None,
            CarriedState::NotReverified(NotReverified::VerifierFailed)
        )]
    );
    Ok(())
}

#[test]
fn a_carried_finding_the_verification_does_not_cover_is_not_re_verified()
-> Result<(), Box<dyn Error>> {
    let runner = Scripted::new([]);
    let (_, verification) = settled(VerifierTier::Micro, 0, &runner)?;
    let carried = [carried(0, Severity::Major)?];

    let outcomes = settle_carried(&carried, &verification);

    assert_eq!(
        shown(&outcomes),
        [(
            Severity::Major,
            None,
            CarriedState::NotReverified(NotReverified::VerifierFailed)
        )]
    );
    Ok(())
}

/// A change whose head holds `src/moved.rs` as `text`.
fn head_with(text: &str) -> Change {
    Change {
        head_files: BTreeMap::from([("src/moved.rs".to_owned(), text.to_owned())]),
        ..Change::default()
    }
}

#[test]
fn micro_keeps_a_carried_finding_whose_anchor_is_present_open_and_advisory()
-> Result<(), Box<dyn Error>> {
    let rechecked = recheck_anchors(
        vec![carried(0, Severity::Blocker)?],
        &head_with("let x = input.parse().unwrap();\n"),
    );
    let bundle = bundle()?;
    let runner = Scripted::new([]);

    let verification = verify(
        &runner,
        request_carrying(VerifierTier::Micro, &[], &rechecked, &bundle),
    );
    let outcomes = settle_carried(&rechecked, &verification);

    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        shown(&outcomes),
        [(
            Severity::Blocker,
            None,
            CarriedState::NotReverified(NotReverified::AnchorPresent)
        )]
    );
    Ok(())
}

#[test]
fn micro_marks_a_carried_finding_whose_anchor_is_gone_as_needing_re_verification()
-> Result<(), Box<dyn Error>> {
    let rechecked = recheck_anchors(
        vec![carried(0, Severity::Blocker)?],
        &head_with("let x = input.parse()?;\n"),
    );
    let bundle = bundle()?;
    let runner = Scripted::new([]);

    let verification = verify(
        &runner,
        request_carrying(VerifierTier::Micro, &[], &rechecked, &bundle),
    );
    let outcomes = settle_carried(&rechecked, &verification);

    assert_eq!(
        shown(&outcomes),
        [(
            Severity::Blocker,
            None,
            CarriedState::NotReverified(NotReverified::AnchorGone)
        )]
    );
    assert_eq!(NotReverified::AnchorGone.label(), "needs re-verification");
    Ok(())
}

#[test]
fn micro_reads_an_unchecked_anchor_as_needing_re_verification() -> Result<(), Box<dyn Error>> {
    let runner = Scripted::new([]);

    let (outcomes, _) = settled(VerifierTier::Micro, 1, &runner)?;

    assert_eq!(
        shown(&outcomes),
        [(
            Severity::Blocker,
            None,
            CarriedState::NotReverified(NotReverified::AnchorGone)
        )]
    );
    Ok(())
}

#[test]
fn a_carried_finding_whose_anchor_is_gone_still_goes_to_the_verifier_in_lite()
-> Result<(), Box<dyn Error>> {
    let rechecked = recheck_anchors(
        vec![carried(0, Severity::Blocker)?],
        &head_with("let x = input.parse()?;\n"),
    );
    let bundle = bundle()?;
    let runner = Scripted::answering([(verdict("refuted", "the unwrap was removed"), 1)]);

    let verification = verify(
        &runner,
        request_carrying(VerifierTier::Lite, &[], &rechecked, &bundle),
    );
    let outcomes = settle_carried(&rechecked, &verification);

    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        outcomes.first().map(|outcome| outcome.verdict),
        Some(Some(VerifierVerdict::Refuted))
    );
    Ok(())
}
