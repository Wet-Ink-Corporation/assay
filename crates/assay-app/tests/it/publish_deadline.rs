//! #332 (#331 F3, #318 F6): every call `publish` makes while holding the per-head lock carries
//! the lock lease's deadline, as `open`'s do since #290, and a lease that runs out under the lock
//! stops every later write: the adapter refuses each call past it, and the ledger refuses the
//! evidence record.

use std::assert_matches;
use std::error::Error;

use assay_app::{ForgeStep, PublishError, Unlock};

use crate::plan_fakes::FakeError;
use crate::publish_fakes::{Call, lease_deadline};
use crate::publish_support::{NAME, Run, stamped, written};

#[test]
fn every_call_under_the_lock_carries_the_lease_s_deadline() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    let by = Some(lease_deadline(1)?);
    let (held, not_held) = world.bounds_held_and_not();
    assert_eq!(
        held,
        [
            (Call::FindCheck, by),
            (Call::RecordEvidence, by),
            (Call::UpdateCheck, by),
            (Call::FindSummary, by),
            (Call::CreateSummary, by),
        ]
    );
    assert_eq!(
        not_held,
        [(Call::FindCheck, None)],
        "only the first read, before the lock, has no deadline"
    );
    Ok(())
}

#[test]
fn a_lease_that_runs_out_under_the_lock_writes_nothing_after() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.expire_after(Call::FindCheck, 2);

    let outcome = run.publish(&world);

    assert_matches!(
        outcome,
        Err(PublishError::Locked {
            step: ForgeStep::UpdateCheck,
            source: FakeError,
            unlock: Unlock::Released,
        })
    );
    assert_eq!(
        world.app_check(NAME)?.output,
        None,
        "the check is unwritten"
    );
    assert!(world.evidence.borrow().is_empty(), "no evidence recorded");
    assert!(world.comments.borrow().is_empty(), "no summary posted");
    assert_eq!(*world.lock.borrow(), None, "the lock is released");
    Ok(())
}
