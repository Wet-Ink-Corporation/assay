//! `admit`'s budget gate run again once the slot is held (06 §1, R3-27, #300): a run the re-gate
//! prices above what its lease reserves gives the slot back and defers, so a rival reading the
//! leases never counts less than this run would reserve; a price at or below the lease's spends.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use assay_app::{
    Admission, CostKind, CostRecord, Deferred, LedgerPath, Role, Spend, TierDegrade, append_cost,
    grant_retry, read_costs,
};
use assay_domain::{Aeu, Tier};
use serde_json::{Value, json};

use crate::admit_fakes::{NOW, OtherCost, World, at, grant_ref, plan, retry_grant};
use crate::lease_fakes::{Before, MemoryRefs};
use crate::ledger_fakes::MemoryLedger;

/// What a `full` run of `bugs`, `security` and `design` reserves: three shards of $2.30.
const FULL: Aeu = Aeu::from_micros(6_900_000);

/// What a `lite` run reserves.
const LITE: Aeu = Aeu::from_micros(740_000);

/// A `final` record of another run on another PR, spending `micros` an hour ago.
fn spent(micros: u64) -> CostRecord {
    OtherCost {
        run_id: 70,
        pr: 99,
        kind: CostKind::Final,
        micros,
        when: "2026-10-19T11:00:00.000Z",
    }
    .record()
}

/// Refs whose slot-0 holder finishes during the first wait: each attempt tries to create slot-0
/// and slot-1, and slot-0 is gone before the second attempt's first creation.
fn slot_0_freed_after_one_wait(on_free: impl FnOnce() + Send + 'static) -> MemoryRefs {
    MemoryRefs::default()
        .rival(Before::Create, |_store| {})
        .rival(Before::Create, |_store| {})
        .rival(Before::Create, move |store| {
            store.remove("refs/heads/slot-0");
            on_free();
        })
}

/// The `budget` of the run description `admit` wrote.
fn budget_of(json: &[u8]) -> Result<Value, Box<dyn Error>> {
    let document: Value = serde_json::from_slice(json)?;
    Ok(document.get("budget").cloned().ok_or("no budget")?)
}

/// The admitted run's spend, or an error naming what was decided instead.
fn spend(admission: &Admission) -> Result<&Spend, Box<dyn Error>> {
    match admission {
        Admission::Spend(spend) => Ok(spend),
        other => Err(format!("not admitted: {other:?}").into()),
    }
}

/// The kind of every cost record in the ledger now.
fn cost_kinds(world: &World) -> Result<Vec<CostKind>, Box<dyn Error>> {
    let costs = read_costs(&world.ledger, at(NOW))?;
    Ok(costs.records().iter().map(|record| record.kind).collect())
}

#[test]
fn a_run_degraded_before_the_wait_that_fits_full_at_the_slot_gives_the_slot_back()
-> Result<(), Box<dyn Error>> {
    let mut world = World {
        refs: slot_0_freed_after_one_wait(|| {}),
        ..World::new()
    }
    .spending(&[spent(30_000_000)])
    .holding_slot(0, 60, "2026-10-19T13:00:00.000Z")
    .holding_slot(1, 61, "2026-10-19T13:00:00.000Z");

    let admitted = world.admit(&plan("full"))?;

    // Before the wait: 30 + two leases of 2.3 + 6.9 is over the cap, + 0.74 is not, so the lease
    // reserves `lite`. At the slot: 30 + 2.3 + 6.9 fits, a `full` price above the lease's.
    assert_eq!(
        admitted.admission(),
        &Admission::Deferred(Deferred::SlotBusy)
    );
    assert_eq!(
        world.refs_held(),
        ["refs/heads/slot-1"],
        "our slot is released"
    );
    assert_eq!(cost_kinds(&world)?, [CostKind::Final], "nothing reserved");
    assert_eq!(
        world.events.kinds(),
        ["SlotReleased", "SlotBusy", "LedgerWritten"]
    );
    assert!(world.deferral().is_some(), "the deferral record is written");
    let budget = budget_of(admitted.run_json())?;
    assert_eq!(
        (&budget["allowed"], &budget["deferral"], &budget["slot"]),
        (&json!(true), &json!("slot-busy"), &Value::Null)
    );
    Ok(())
}

#[test]
fn a_grant_recorded_during_the_wait_gives_the_slot_back_and_stays_waiting()
-> Result<(), Box<dyn Error>> {
    let scratch = MemoryRefs::default();
    grant_retry(&scratch, &retry_grant(Role::Combined))?;
    let granted = grant_ref(Role::Combined);
    let (_, message) = scratch
        .get(&granted)
        .ok_or("the scratch refs hold the grant")?;
    // The rival owns its copy of the name; the assertion keeps the other.
    let recorded = granted.clone();
    let refs = MemoryRefs::default().rival(Before::Create, move |store| {
        store.create(&recorded, &message);
    });
    let mut world = World {
        refs,
        ..World::new()
    };

    let admitted = world.admit(&plan("lite"))?;

    // The lease reserves 0.74; at the slot the grant raises `combined` to 0.99.
    assert_eq!(
        admitted.admission(),
        &Admission::Deferred(Deferred::SlotBusy)
    );
    assert_eq!(world.refs_held(), [granted], "slot gone, grant waiting");
    assert_eq!(
        cost_kinds(&world)?,
        Vec::<CostKind>::new(),
        "nothing reserved"
    );
    Ok(())
}

#[test]
fn a_run_priced_below_its_lease_at_the_slot_spends_at_the_lower_price() -> Result<(), Box<dyn Error>>
{
    let month = at(NOW).month();
    let earlier = spent(27_000_000);
    let during_the_wait = OtherCost {
        run_id: 71,
        pr: 98,
        kind: CostKind::Reserved,
        micros: 5_000_000,
        when: "2026-10-19T12:00:20.000Z",
    }
    .record();
    let scratch = MemoryLedger::default();
    append_cost(&scratch, &earlier)?;
    append_cost(&scratch, &during_the_wait)?;
    let costs = LedgerPath::costs(month);
    let both = scratch
        .file(&costs)
        .ok_or("the scratch ledger holds both")?;
    let trigger = Arc::new(AtomicBool::new(false));
    let landed = Arc::clone(&trigger);
    let mut world = World {
        ledger: MemoryLedger::default().staging(trigger, &costs, &both),
        refs: slot_0_freed_after_one_wait(move || landed.store(true, Ordering::Relaxed)),
        ..World::new()
    }
    .spending(&[earlier])
    .holding_slot(0, 60, "2026-10-19T13:00:00.000Z")
    .holding_slot(1, 61, "2026-10-19T13:00:00.000Z");

    let admitted = world.admit(&plan("full"))?;

    // Before the wait: 27 + 4.6 + 6.9 fits, so the lease reserves `full`. At the slot:
    // 27 + 5 + 2.3 + 6.9 is over the cap, + 0.74 is not: `lite`, below the lease's.
    let spent = spend(admitted.admission())?;
    assert_eq!(
        (spent.tier, spent.reserved, spent.hold.lease.reserved_aeu),
        (Tier::Lite, LITE, FULL)
    );
    assert_eq!(admitted.degrade(), TierDegrade::FullToLite);
    assert_eq!(
        cost_kinds(&world)?,
        [CostKind::Final, CostKind::Reserved, CostKind::Reserved]
    );
    Ok(())
}

#[test]
fn a_run_priced_at_its_leases_amount_at_the_slot_spends() -> Result<(), Box<dyn Error>> {
    let mut world = World::new();

    let admitted = world.admit(&plan("lite"))?;

    let spent = spend(admitted.admission())?;
    assert_eq!(
        (spent.reserved, spent.hold.lease.reserved_aeu),
        (LITE, LITE)
    );
    assert_eq!(world.events.kinds(), ["LedgerWritten", "Admitted"]);
    Ok(())
}
