//! `admit` (S1a, 04 §1, §2.4, 06 §1): the budget gate, the slot, the reservation and what the run
//! description gains, against an in-memory ledger and ref store.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    Admission, AdmitError, Budget, Contract, ContractValidator, CostKind, CostRecord, CostRole,
    LedgerPath, Role, RoleCost, Slot, Spend, TierDegrade, read_costs,
};
use assay_domain::{Aeu, NotRunReason, Tier};
use serde_json::{Value, json};

use crate::admit_fakes::{NOW, PR, REPO, World, at, head, nz, plan, run_key, this_run};

/// Slot `index`.
fn slot(index: u8) -> Result<Slot, Box<dyn Error>> {
    Ok(Slot::new(index).ok_or("slots 0 and 1 exist")?)
}

/// `micros` millionths of an AEU.
const fn aeu(micros: u64) -> Aeu {
    Aeu::from_micros(micros)
}

/// The `budget` object of the run description `admit` wrote.
fn budget_of(json: &[u8]) -> Result<Value, Box<dyn Error>> {
    let document: Value = serde_json::from_slice(json)?;
    Ok(document.get("budget").cloned().ok_or("no budget")?)
}

/// The `reserved` record a `lite` run of this run writes at `NOW`.
fn lite_reserved() -> Result<CostRecord, Box<dyn Error>> {
    Ok(CostRecord {
        run_uid: this_run(1),
        run_key: run_key().parse()?,
        repo: REPO.to_owned(),
        pr: nz(PR),
        head_sha: head().parse()?,
        tier: Tier::Lite,
        roles: vec![
            RoleCost {
                role: CostRole::Role(Role::Combined),
                aeu: aeu(500_000),
            },
            RoleCost {
                role: CostRole::Verifier,
                aeu: aeu(240_000),
            },
        ],
        aeu: aeu(740_000),
        kind: CostKind::Reserved,
        cancelled: false,
        at: at(NOW),
    })
}

#[test]
fn a_re_run_is_refused_before_anything_is_read() {
    let mut world = World::unreachable();

    let refused = world.admit_as(this_run(2), &plan("lite").bytes());

    assert_matches!(refused, Err(AdmitError::NotFirstAttempt { attempt }) if attempt.get() == 2);
}

#[test]
fn a_draft_takes_no_slot_and_reserves_nothing() -> Result<(), Box<dyn Error>> {
    let mut world = World::unreachable();
    let draft = plan("lite").draft();

    let admitted = world.admit(&draft)?;

    assert_eq!(
        admitted.admission(),
        &Admission::NoSpend(NotRunReason::Draft)
    );
    assert_eq!(admitted.run_json(), draft.bytes());
    assert_eq!((admitted.budget(), admitted.pack()), (None, None));
    Ok(())
}

#[test]
fn a_tier_without_an_llm_stage_takes_no_slot_and_reserves_nothing() -> Result<(), Box<dyn Error>> {
    for tier in ["trivial", "needs-split"] {
        let mut world = World::unreachable();
        let planned = plan(tier);

        let admitted = world.admit(&planned)?;

        assert_eq!(
            admitted.admission(),
            &Admission::NoSpend(NotRunReason::NoLlmTier),
            "{tier}"
        );
        assert_eq!(admitted.run_json(), planned.bytes(), "{tier}");
    }
    Ok(())
}

#[test]
fn a_lite_run_within_budget_takes_a_slot_and_reserves_its_run_cap() -> Result<(), Box<dyn Error>> {
    let mut world = World::new();

    let admitted = world.admit(&plan("lite"))?;

    let Admission::Spend(Spend {
        hold,
        tier,
        reserved,
        retry,
    }) = admitted.admission()
    else {
        return Err(format!("not admitted: {:?}", admitted.admission()).into());
    };
    assert_eq!(
        (hold.slot, *tier, *reserved, *retry),
        (slot(0)?, Tier::Lite, aeu(740_000), None)
    );
    assert_eq!(hold.lease.reserved_aeu, aeu(740_000));
    let written = read_costs(&world.ledger, at(NOW))?;
    assert_eq!(written.records(), [lite_reserved()?]);
    assert_eq!(world.events.kinds(), ["LedgerWritten", "Admitted"]);
    Ok(())
}

#[test]
fn an_admitted_run_reports_its_budget() -> Result<(), Box<dyn Error>> {
    let mut world = World::new();

    let admitted = world.admit(&plan("lite"))?;

    assert_eq!(
        admitted.budget(),
        Some(&Budget {
            day_spent: Aeu::ZERO,
            allowed: true,
            slot: Some(slot(0)?),
            deferred: None,
            degrade: TierDegrade::None,
            reused: None,
            retry: None,
        })
    );
    assert_eq!(
        budget_of(admitted.run_json())?,
        json!({
            "ledger_day_spent_aeu": 0.0,
            "day_cap_aeu": 40.0,
            "window_state": "unobserved",
            "allowed": true,
            "slot": "slot-0",
            "deferral": "none",
        })
    );
    let validator = ContractValidator::new()?;
    validator.read(Contract::Run, admitted.run_json())?;
    Ok(())
}

#[test]
fn an_unreadable_ledger_fails_admit_with_nothing_taken() {
    let mut world = World {
        ledger: crate::ledger_fakes::MemoryLedger::default().down(),
        ..World::new()
    };

    let failed = world.admit(&plan("lite"));

    assert_matches!(failed, Err(AdmitError::LedgerRead(_)));
    assert_eq!(world.refs.names(), Vec::<String>::new());
    assert_eq!(world.ledger.writes(), []);
    assert!(
        world
            .ledger
            .file(&LedgerPath::costs(at(NOW).month()))
            .is_none(),
        "no reservation"
    );
}
