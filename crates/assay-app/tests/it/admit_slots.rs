//! `admit`'s slot and retry grant (06 §1, R3-06, R3-14): expired leases reaped before any wait,
//! the ten-minute wait and `slot-busy`, a grant consumed by compare-and-set only by a run that
//! spends, and the slot released when a later step fails.

use std::assert_matches;
use std::error::Error;
use std::time::Duration;

use assay_app::{
    Admission, AdmitError, AfterSlot, Deferred, LedgerPath, LedgerWriteError, Role, Spend, Unwound,
};
use assay_domain::{Aeu, EventPayload, RunUid};
use serde_json::{Value, json};

use crate::admit_fakes::{NOW, World, at, grant_ref, nz, plan};
use crate::lease_fakes::{Before, MemoryRefs};
use crate::ledger_fakes::MemoryLedger;

/// The run of repository 42 whose lease `lease_line(run_id, ..)` writes.
fn lease_run(run_id: u64) -> RunUid {
    RunUid {
        repo_id: nz(42),
        run_id: nz(run_id),
        run_attempt: nz(1),
    }
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

#[test]
fn an_expired_lease_is_reaped_before_any_wait() -> Result<(), Box<dyn Error>> {
    let mut world = World::new()
        .holding_slot(0, 60, "2026-10-19T11:00:00.000Z")
        .holding_slot(1, 61, "2026-10-19T12:30:00.000Z");

    let admitted = world.admit(&plan("lite"))?;

    assert_eq!(spend(admitted.admission())?.hold.slot.index(), 0);
    assert_eq!(world.clock.sleeps, []);
    assert_eq!(
        world.events.kinds(),
        ["SlotReaped", "LedgerWritten", "Admitted"]
    );
    assert_eq!(
        world.events.events.first().map(|event| &event.payload),
        Some(&EventPayload::SlotReaped {
            run_uid: lease_run(60)
        })
    );
    Ok(())
}

#[test]
fn with_every_slot_live_admit_waits_ten_minutes_then_defers_slot_busy() -> Result<(), Box<dyn Error>>
{
    let mut world = World::new()
        .holding_slot(0, 60, "2026-10-19T13:00:00.000Z")
        .holding_slot(1, 61, "2026-10-19T13:00:00.000Z");

    let admitted = world.admit(&plan("lite"))?;

    assert_eq!(
        admitted.admission(),
        &Admission::Deferred(Deferred::SlotBusy)
    );
    assert_eq!(world.clock.sleeps, [Duration::from_secs(30); 20]);
    assert_eq!(world.events.kinds(), ["SlotBusy", "LedgerWritten"]);
    assert_eq!(
        world.ledger.file(&LedgerPath::costs(at(NOW).month())),
        None,
        "nothing reserved"
    );
    assert!(world.deferral().is_some(), "the deferral record is written");
    assert_eq!(
        (
            &budget_of(admitted.run_json())?["allowed"],
            &budget_of(admitted.run_json())?["deferral"]
        ),
        (&json!(true), &json!("slot-busy"))
    );
    Ok(())
}

#[test]
fn a_slot_freed_during_the_wait_is_taken() -> Result<(), Box<dyn Error>> {
    // Each attempt tries to create slot-0 and slot-1; slot-0 is freed before the fourth attempt.
    let mut refs = MemoryRefs::default();
    for _ in 0..6 {
        refs = refs.rival(Before::Create, |_store| {});
    }
    let refs = refs.rival(Before::Create, |store| store.remove("refs/heads/slot-0"));
    let mut world = World {
        refs,
        ..World::new()
    }
    .holding_slot(0, 60, "2026-10-19T13:00:00.000Z")
    .holding_slot(1, 61, "2026-10-19T13:00:00.000Z");

    let admitted = world.admit(&plan("lite"))?;

    assert_eq!(spend(admitted.admission())?.hold.slot.index(), 0);
    assert_eq!(world.clock.sleeps, [Duration::from_secs(30); 3]);
    Ok(())
}

#[test]
fn a_waiting_grant_is_consumed_and_raises_its_roles_cap() -> Result<(), Box<dyn Error>> {
    let mut world = World::new().granting(Role::Combined);

    let admitted = world.admit(&plan("lite"))?;

    let spent = spend(admitted.admission())?;
    assert_eq!(
        (spent.retry, spent.reserved),
        (Some(Role::Combined), Aeu::from_micros(990_000))
    );
    assert!(
        !world.refs_held().contains(&grant_ref(Role::Combined)),
        "the grant is consumed"
    );
    assert_eq!(
        budget_of(admitted.run_json())?["retry_grant"],
        json!({ "role": "combined" })
    );
    Ok(())
}

#[test]
fn a_grant_another_run_consumed_first_raises_nothing() -> Result<(), Box<dyn Error>> {
    let taken = grant_ref(Role::Combined);
    let mut world = World {
        refs: MemoryRefs::default().rival(Before::Delete, move |store| store.remove(&taken)),
        ..World::new()
    }
    .granting(Role::Combined);

    let admitted = world.admit(&plan("lite"))?;

    let spent = spend(admitted.admission())?;
    // The reservation was made for the grant before it was lost: it stays the higher one.
    assert_eq!(
        (spent.retry, spent.reserved),
        (None, Aeu::from_micros(990_000))
    );
    assert_eq!(budget_of(admitted.run_json())?.get("retry_grant"), None);
    Ok(())
}

#[test]
fn a_grant_for_a_role_the_run_does_not_have_is_left_waiting() -> Result<(), Box<dyn Error>> {
    let mut world = World::new().granting(Role::Bugs);

    let admitted = world.admit(&plan("lite"))?;

    let spent = spend(admitted.admission())?;
    assert_eq!(
        (spent.retry, spent.reserved),
        (None, Aeu::from_micros(740_000))
    );
    assert!(world.refs_held().contains(&grant_ref(Role::Bugs)));
    Ok(())
}

#[test]
fn a_run_deferred_for_a_busy_slot_leaves_its_grant_waiting() -> Result<(), Box<dyn Error>> {
    let mut world = World::new()
        .granting(Role::Combined)
        .holding_slot(0, 60, "2026-10-19T13:00:00.000Z")
        .holding_slot(1, 61, "2026-10-19T13:00:00.000Z");

    let admitted = world.admit(&plan("lite"))?;

    assert_eq!(
        admitted.admission(),
        &Admission::Deferred(Deferred::SlotBusy)
    );
    assert!(world.refs_held().contains(&grant_ref(Role::Combined)));
    Ok(())
}

#[test]
fn a_failure_after_the_slot_is_taken_releases_it() {
    let mut world = World {
        ledger: MemoryLedger::default().rival_always(),
        ..World::new()
    };

    let failed = world.admit(&plan("lite"));

    let Err(AdmitError::AfterSlot(failed)) = failed else {
        panic!("expected a failure after the slot, got {failed:?}");
    };
    assert_matches!(
        *failed,
        AfterSlot {
            release: None,
            cause: Unwound::Reserve(LedgerWriteError::Contended { .. }),
            ..
        }
    );
    assert_eq!(failed.hold.slot.index(), 0);
    assert_eq!(world.refs_held(), Vec::<String>::new());
}
