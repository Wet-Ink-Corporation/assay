//! A persisted refusal (#60 ST-14, D5 A): `assay decide` records a refusal's reason, and
//! `assay publish` publishes it with [`publish_refusal`] exactly as `publish` publishes the
//! refusal itself.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    CheckConclusion, CheckState, JobResult, MAX_REFUSAL_DETAIL_BYTES, NotARefusal, Publication,
    Published, RefusalReason, Verdict, publish, publish_refusal,
};
use assay_domain::{Fingerprint, Reason, ReasonCode};

use crate::decide_fakes::{Scenario, refusal};
use crate::publish_fakes::Call;
use crate::publish_support::{NAME, Run, ports, stamped};

/// The refusal of a run whose plan job failed: `gate-error`, `plan: failure`.
fn plan_failed() -> Result<assay_app::Refusal, Box<dyn Error>> {
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
