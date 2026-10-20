//! `admit`'s budget gate and evidence reuse (06 §1, 04 §2.4, 02 §5): the day's cap, the PR's
//! hourly admissions, the full-to-lite degrade, the deferral record, and the evidence records
//! that may and may not stand in for the LLM stage.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    Admission, CostKind, DeferralRecord, Deferred, EvidenceRecord, LedgerPath, LlmEvidence, Spend,
    TierDegrade, read_costs,
};
use assay_domain::{Aeu, Tier};
use serde_json::{Value, json};

use crate::admit_fakes::{NOW, OtherCost, PR, REPO, World, at, head, nz, plan, run_key};

/// A `final` record of another run on another PR, spending `micros` an hour ago.
fn spent(micros: u64) -> assay_app::CostRecord {
    OtherCost {
        run_id: 70,
        pr: 99,
        kind: CostKind::Final,
        micros,
        when: "2026-10-19T11:00:00.000Z",
    }
    .record()
}

/// `reserved` records of runs 71 to 73 on the PR, each written at `when`.
fn three_admissions(when: &str) -> [assay_app::CostRecord; 3] {
    [71, 72, 73].map(|run_id| {
        OtherCost {
            run_id,
            pr: PR,
            kind: CostKind::Reserved,
            micros: 250_000,
            when,
        }
        .record()
    })
}

/// The run description `admit` wrote, parsed.
fn document(json: &[u8]) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(json)?)
}

/// The deferral record a first deferral of the PR's head writes.
fn new_deferral() -> Result<DeferralRecord, Box<dyn Error>> {
    Ok(DeferralRecord {
        repo: REPO.to_owned(),
        pr: nz(PR),
        head: head().parse()?,
        count: 0,
        resets_at: None,
        flag: false,
    })
}

#[test]
fn a_classless_full_run_over_the_cap_is_degraded_to_lite() -> Result<(), Box<dyn Error>> {
    let mut world = World::new().spending(&[spent(38_000_000)]);

    let admitted = world.admit(&plan("full"))?;

    let Admission::Spend(Spend { tier, reserved, .. }) = admitted.admission() else {
        return Err(format!("not admitted: {:?}", admitted.admission()).into());
    };
    assert_eq!(
        (*tier, *reserved, admitted.degrade()),
        (
            Tier::Lite,
            Aeu::from_micros(740_000),
            TierDegrade::FullToLite
        )
    );
    let written = document(admitted.run_json())?;
    assert_eq!(
        (
            &written["tier"],
            &written["roles"],
            &written["budget"]["tier_degraded_from"]
        ),
        (&json!("lite"), &json!(["combined"]), &json!("full"))
    );
    let reserved = read_costs(&world.ledger, at(NOW))?;
    assert_eq!(
        reserved
            .records()
            .iter()
            .filter(|record| record.kind == CostKind::Reserved)
            .map(|record| record.tier)
            .collect::<Vec<_>>(),
        [Tier::Lite]
    );
    Ok(())
}

#[test]
fn a_trust_path_run_over_the_cap_is_deferred_and_never_degraded() -> Result<(), Box<dyn Error>> {
    let mut world = World::new().spending(&[spent(38_000_000)]);

    let admitted = world.admit(&plan("full").trust_path())?;

    assert_eq!(admitted.admission(), &Admission::Deferred(Deferred::Budget));
    assert_eq!(world.refs_held(), Vec::<String>::new());
    assert_eq!(world.deferral(), Some(new_deferral()?));
    assert_eq!(world.events.kinds(), ["BudgetDeferred", "LedgerWritten"]);
    let written = document(admitted.run_json())?;
    assert_eq!(
        (&written["tier"], &written["budget"]),
        (
            &json!("full"),
            &json!({
                "ledger_day_spent_aeu": 38.0,
                "day_cap_aeu": 40.0,
                "window_state": "unobserved",
                "allowed": false,
                "slot": null,
                "deferral": "budget-deferred",
            })
        )
    );
    Ok(())
}

#[test]
fn a_run_over_the_cap_even_as_lite_is_deferred() -> Result<(), Box<dyn Error>> {
    let mut world = World::new().spending(&[spent(39_500_000)]);

    let admitted = world.admit(&plan("full"))?;

    assert_eq!(admitted.admission(), &Admission::Deferred(Deferred::Budget));
    assert_eq!(admitted.degrade(), TierDegrade::None);
    Ok(())
}

#[test]
fn a_pr_admitted_three_times_in_the_hour_is_deferred() -> Result<(), Box<dyn Error>> {
    let mut world = World::new().spending(&three_admissions("2026-10-19T11:30:00.000Z"));

    let admitted = world.admit(&plan("micro"))?;

    assert_eq!(admitted.admission(), &Admission::Deferred(Deferred::Budget));
    assert_eq!(world.refs_held(), Vec::<String>::new());
    Ok(())
}

#[test]
fn admissions_before_the_hour_do_not_count() -> Result<(), Box<dyn Error>> {
    let mut world = World::new().spending(&three_admissions("2026-10-19T10:59:59.999Z"));

    let admitted = world.admit(&plan("micro"))?;

    assert_matches!(admitted.admission(), Admission::Spend(_));
    Ok(())
}

/// An evidence record of the plan's evidence key, `llm` at `tier`.
fn record(llm: LlmEvidence, tier: Tier) -> Result<EvidenceRecord, Box<dyn Error>> {
    Ok(EvidenceRecord {
        evidence_key: run_key(),
        bundle_sha256: format!("sha256:{}", "9".repeat(64)).parse()?,
        llm,
        tier,
    })
}

#[test]
fn a_complete_record_at_the_needed_tier_is_reused_with_nothing_reserved()
-> Result<(), Box<dyn Error>> {
    let reused = record(LlmEvidence::Complete, Tier::Full)?;
    let kept = reused.clone();
    let mut world = World::new().with_state(|state| state.evidence_records = vec![kept]);

    let admitted = world.admit(&plan("lite"))?;

    assert_eq!(admitted.admission(), &Admission::Reused(reused));
    assert_eq!(world.refs_held(), Vec::<String>::new());
    assert_eq!(
        world.ledger.file(&LedgerPath::costs(at(NOW).month())),
        None,
        "nothing reserved"
    );
    assert_eq!(world.events.kinds(), ["EvidenceReused"]);
    assert_eq!(
        document(admitted.run_json())?["budget"]["reused_evidence"],
        json!({
            "evidence_key": run_key(),
            "bundle_sha256": "9".repeat(64),
            "llm": "complete",
            "tier": "full",
        })
    );
    Ok(())
}

#[test]
fn no_draft_deferred_degraded_lower_or_other_record_stands_in() -> Result<(), Box<dyn Error>> {
    let mut other_key = record(LlmEvidence::Complete, Tier::Full)?;
    other_key.evidence_key = format!("sha256:{}", "8".repeat(64));
    let records = [
        record(LlmEvidence::SkippedDraft, Tier::Full)?,
        record(LlmEvidence::Deferred, Tier::Full)?,
        record(LlmEvidence::Degraded, Tier::Full)?,
        record(LlmEvidence::Complete, Tier::Lite)?,
        record(LlmEvidence::Complete, Tier::Trivial)?,
        other_key,
    ];
    for held in records {
        let shown = format!("{held:?}");
        let mut world = World::new().with_state(|state| state.evidence_records = vec![held]);

        let admitted = world.admit(&plan("full"))?;

        assert_matches!(
            admitted.admission(),
            Admission::Spend(_),
            "{shown} stood in"
        );
    }
    Ok(())
}

#[test]
fn a_deferral_of_the_same_head_keeps_its_count() -> Result<(), Box<dyn Error>> {
    let requeued = DeferralRecord {
        count: 2,
        flag: true,
        ..new_deferral()?
    };
    let kept = requeued.clone();
    let mut world = World::new()
        .spending(&[spent(39_900_000)])
        .with_state(|state| state.deferral = Some(kept));

    world.admit(&plan("micro"))?;

    assert_eq!(world.deferral(), Some(requeued));
    assert_eq!(world.events.kinds(), ["BudgetDeferred"]);
    Ok(())
}

#[test]
fn a_deferral_of_a_new_head_counts_from_zero() -> Result<(), Box<dyn Error>> {
    let older = DeferralRecord {
        head: "f".repeat(40).parse()?,
        count: 2,
        ..new_deferral()?
    };
    let mut world = World::new()
        .spending(&[spent(39_900_000)])
        .with_state(|state| state.deferral = Some(older));

    world.admit(&plan("micro"))?;

    assert_eq!(world.deferral(), Some(new_deferral()?));
    Ok(())
}
