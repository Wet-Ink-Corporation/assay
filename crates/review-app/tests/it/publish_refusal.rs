//! A persisted refusal (#60 ST-14, D5 A): `assay decide` records a refusal's reason, and
//! `assay publish` publishes it with [`publish_refusal`] exactly as `publish` publishes the
//! refusal itself.

use std::assert_matches;
use std::error::Error;

use review_app::{
    ArtifactJob, CheckConclusion, CheckState, JobResult, MAX_REFUSAL_DETAIL_BYTES, NotARefusal,
    OutputsProblem, PlanMismatch, PlanRefusal, Publication, Published, Refusal, RefusalReason,
    Unreadable, UpstreamJob, Verdict, publish, publish_refusal,
};
use review_domain::{Fingerprint, Reason, ReasonCode, Tier};

use crate::decide_fakes::{Scenario, refusal};
use crate::publish_fakes::Call;
use crate::publish_support::{NAME, Run, ports, stamped};

/// The refusal of a run whose plan job failed: `gate-error`, `plan: failure`.
fn plan_failed() -> Result<Refusal, Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.plan_job.result = JobResult::Failure;
    refusal(scenario.authenticate()?)
}

#[test]
fn a_persisted_refusal_publishes_the_check_the_refusal_itself_would() -> Result<(), Box<dyn Error>>
{
    let refused = plan_failed()?;
    let run = Run::new()?;
    let direct = stamped()?;
    publish(
        &Publication {
            target: run.target(),
            verdict: Verdict::Refused(&refused),
        },
        &ports(&direct),
    )?;
    let persisted = stamped()?;

    let outcome = publish_refusal(
        &run.target(),
        &RefusalReason::from(&refused),
        &persisted,
        &persisted,
    );

    let Ok(Published::Written(written)) = outcome else {
        return Err(format!("expected a write, got {outcome:?}").into());
    };
    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    let (persisted_check, direct_check) = (persisted.app_check(NAME)?, direct.app_check(NAME)?);
    assert_eq!(persisted_check.external_id, direct_check.external_id);
    assert_eq!(persisted_check.status, direct_check.status);
    assert_eq!(persisted_check.output, direct_check.output);
    assert_eq!(
        persisted.writes(),
        [
            Call::Acquire,
            // D2 of #61 (wi-a99de2): a refusal records the publication register alone.
            Call::RecordPublished,
            Call::UpdateCheck,
            Call::CreateSummary,
            Call::Release
        ],
        "no upload and no evidence"
    );
    Ok(())
}

#[test]
fn a_refusal_reason_reads_back_from_its_reason() -> Result<(), Box<dyn Error>> {
    let reason = RefusalReason::from(&plan_failed()?);

    // `try_from` consumes its reason; the original stays to compare with.
    let read = RefusalReason::try_from(reason.reason().clone());

    assert_eq!(read.as_ref(), Ok(&reason));
    assert_eq!(reason.reason().code, ReasonCode::GateError);
    assert_eq!(reason.reason().detail, "plan: failure");
    Ok(())
}

#[test]
fn a_reason_no_refusal_gives_is_handed_back() -> Result<(), Box<dyn Error>> {
    let fp: Fingerprint = "0123456789abcdef".parse()?;
    let refusals = [
        Reason {
            code: ReasonCode::PersistError,
            detail: "upload".to_owned(),
            fp: None,
        },
        Reason {
            code: ReasonCode::GateError,
            detail: "temper".to_owned(),
            fp: Some(fp),
        },
        Reason {
            code: ReasonCode::PlanMismatch,
            detail: "tier\nforged".to_owned(),
            fp: None,
        },
        Reason {
            code: ReasonCode::GateError,
            detail: "x".repeat(MAX_REFUSAL_DETAIL_BYTES + 1),
            fp: None,
        },
        Reason {
            code: ReasonCode::GateError,
            detail: String::new(),
            fp: None,
        },
    ];
    for reason in refusals {
        // The original stays to compare with what comes back.
        assert_matches!(
            RefusalReason::try_from(reason.clone()),
            Err(NotARefusal { reason: back }) if back == reason
        );
    }
    let at_cap = Reason {
        code: ReasonCode::PlanMismatch,
        detail: "x".repeat(MAX_REFUSAL_DETAIL_BYTES),
        fp: None,
    };
    assert_matches!(RefusalReason::try_from(at_cap), Ok(_));
    Ok(())
}

/// One refusal of each kind, plan mismatches included, with details as long, empty or
/// control-laden as a file or field name could make them.
fn one_of_each() -> Vec<Refusal> {
    // One byte, then two-byte characters, so the byte cap falls inside a character.
    let wide = format!("x{}", "é".repeat(MAX_REFUSAL_DETAIL_BYTES));
    let refusals = vec![
        Refusal::PlanNotSucceeded {
            result: JobResult::Failure,
        },
        Refusal::Cancelled {
            job: UpstreamJob::Review,
        },
        Refusal::ArtifactDigest {
            job: ArtifactJob::GatesLint,
            file: "temper.json\n::warning::forged".to_owned(),
        },
        Refusal::ArtifactUnreadable {
            job: ArtifactJob::Admit,
            file: "x".repeat(MAX_REFUSAL_DETAIL_BYTES * 2),
            why: Unreadable::TooLarge,
        },
        Refusal::Outputs {
            job: UpstreamJob::GatesTest,
            problem: OutputsProblem::FileName,
        },
        Refusal::PlanMismatch(PlanMismatch::Invalid),
        Refusal::PlanMismatch(PlanMismatch::ArtifactNames),
        Refusal::PlanMismatch(PlanMismatch::Field {
            name: String::new(),
        }),
        Refusal::PlanMismatch(PlanMismatch::Field { name: wide }),
        Refusal::PlanMismatch(PlanMismatch::Field {
            name: "tier\u{7f}\u{1b}[2J".to_owned(),
        }),
        Refusal::PlanMismatch(PlanMismatch::Recompute(PlanRefusal::DiffTooSlow)),
        Refusal::PlanMismatch(PlanMismatch::Degrade {
            tier: Tier::Lite,
            classes: Vec::new(),
        }),
    ];
    // No wildcard: a new kind of refusal does not compile until it is listed above.
    for refusal in &refusals {
        match refusal {
            Refusal::PlanNotSucceeded { .. }
            | Refusal::Cancelled { .. }
            | Refusal::ArtifactDigest { .. }
            | Refusal::ArtifactUnreadable { .. }
            | Refusal::Outputs { .. }
            | Refusal::PlanMismatch(
                PlanMismatch::Invalid
                | PlanMismatch::ArtifactNames
                | PlanMismatch::Field { .. }
                | PlanMismatch::Recompute(_)
                | PlanMismatch::Degrade { .. },
            ) => {}
        }
    }
    refusals
}

#[test]
fn every_refusal_s_reason_reads_back_whatever_its_details() {
    for refusal in one_of_each() {
        let reason = RefusalReason::from(&refusal);

        let read = RefusalReason::try_from(reason.reason().clone());

        assert_eq!(read.as_ref(), Ok(&reason), "{refusal:?}");
        assert_eq!(reason.reason().code, refusal.reason().code, "{refusal:?}");
    }
}

#[test]
fn each_refusal_publishes_its_detail_kept_or_cleaned_exactly() {
    let cut = |prefix: &str| {
        let fill = MAX_REFUSAL_DETAIL_BYTES - prefix.len();
        format!("{prefix}{}", "x".repeat(fill))
    };
    // In `one_of_each`'s order; `None` is a detail already plain, kept as given.
    let cleaned = [
        None,
        None,
        Some("artifact-digest: gates-lint/temper.json\u{FFFD}::warning::forged".to_owned()),
        Some(cut("document-too-large: admit/")),
        None,
        None,
        None,
        Some("unnamed".to_owned()),
        // One byte, then two-byte characters: the cap falls inside the 512th, which is dropped.
        Some(format!("x{}", "é".repeat(511))),
        Some("tier\u{FFFD}\u{FFFD}[2J".to_owned()),
        None,
        None,
    ];
    let refusals = one_of_each();
    assert_eq!(refusals.len(), cleaned.len());

    for (refusal, cleaned) in refusals.iter().zip(cleaned) {
        let given = refusal.reason().detail;

        let published = RefusalReason::from(refusal).reason().detail.clone();

        assert!(published.len() <= MAX_REFUSAL_DETAIL_BYTES, "{refusal:?}");
        match cleaned {
            None => assert_eq!(published, given, "{refusal:?}"),
            Some(cleaned) => {
                assert_ne!(cleaned, given, "{refusal:?}");
                assert_eq!(published, cleaned, "{refusal:?}");
            }
        }
    }
}

#[test]
fn every_refusal_publishes_the_same_check_persisted_or_direct() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    for refused in one_of_each() {
        let direct = stamped()?;
        publish(
            &Publication {
                target: run.target(),
                verdict: Verdict::Refused(&refused),
            },
            &ports(&direct),
        )?;
        let persisted = stamped()?;

        publish_refusal(
            &run.target(),
            &RefusalReason::from(&refused),
            &persisted,
            &persisted,
        )?;

        let (persisted_check, direct_check) = (persisted.app_check(NAME)?, direct.app_check(NAME)?);
        assert_eq!(
            persisted_check.external_id, direct_check.external_id,
            "{refused:?}"
        );
        assert_eq!(persisted_check.status, direct_check.status, "{refused:?}");
        assert_eq!(persisted_check.output, direct_check.output, "{refused:?}");
    }
    Ok(())
}
