//! Taking a review slot (06 §1, R3-06): each attempt reaps the expired leases on the slots it
//! tries, by compare-and-set, before it finds them busy, so `admit` never waits behind a dead
//! review; with every slot held by a live lease it waits and tries again, up to ten minutes.

use std::time::Duration;

use assay_domain::RunUid;

use super::clock_time;
use crate::clock::Clock;
use crate::ledger::{LeaseError, LedgerRefs, SlotAcquired, SlotHold, SlotRequest, acquire_slot};

/// How long `admit` waits between attempts at a slot.
pub const SLOT_POLL: Duration = Duration::from_secs(30);

/// How many times `admit` waits [`SLOT_POLL`] before it defers with `slot-busy`: ten minutes in
/// all (06 §1).
pub const SLOT_POLLS: u32 = 20;

/// What the attempts at a slot did.
#[derive(Debug)]
pub(super) struct Taken {
    /// The slot held, or none after the wait.
    pub(super) hold: Option<SlotHold>,
    /// The runs whose expired leases the attempts reaped.
    pub(super) reaped: Vec<RunUid>,
}

/// Why no attempt could be made.
#[derive(Debug)]
pub(super) enum SlotFailure<E> {
    /// The refs could not be read or written, or a lease cannot be read.
    Lease(LeaseError<E>),
    /// The clock reads outside 1970 to 9999.
    Clock,
}

/// Takes a slot for `request`: tries at once, then after each of [`SLOT_POLLS`] waits of
/// [`SLOT_POLL`]. Each attempt's lease expires its TTL after the attempt's own clock reading.
pub(super) fn take_slot<R: LedgerRefs, K: Clock>(
    refs: &R,
    clock: &mut K,
    request: &SlotRequest,
) -> Result<Taken, SlotFailure<R::Error>> {
    let mut reaped = Vec::new();
    for poll in 0..=SLOT_POLLS {
        if poll > 0 {
            clock.sleep(SLOT_POLL);
        }
        let now = clock_time(clock).map_err(|_range| SlotFailure::Clock)?;
        let acquisition = acquire_slot(refs, request, now).map_err(SlotFailure::Lease)?;
        reaped.extend(acquisition.reaped.iter().map(|view| view.lease.run_uid));
        if let SlotAcquired::Held(hold) = acquisition.acquired {
            return Ok(Taken {
                hold: Some(hold),
                reaped,
            });
        }
    }
    Ok(Taken { hold: None, reaped })
}

#[cfg(test)]
mod tests {
    use super::{SLOT_POLL, SLOT_POLLS};
    use crate::ledger::SLOT_TTL;

    #[test]
    fn the_wait_lasts_ten_minutes_and_ends_inside_a_lease() {
        let waited = SLOT_POLL * SLOT_POLLS;

        assert_eq!(waited.as_secs(), 600);
        assert!(waited < SLOT_TTL, "a live lease outlasts the wait");
    }
}
