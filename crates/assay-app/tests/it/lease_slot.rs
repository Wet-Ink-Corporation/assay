//! The review slots (06 §1, R3-06, R3-16): taken by creating `refs/heads/slot-N` at a commit
//! carrying the lease, released and reaped only by compare-and-set.

use std::sync::Barrier;
use std::thread;

use assay_app::{
    Corruption, LeaseError, LeaseRef, LeaseReservation, LeaseState, MAX_LEASE_MESSAGE_BYTES,
    MAX_REVIEW_TIMEOUT, Released, SLOT_TTL, Slot, SlotAcquired, acquire_slot, read_slots,
    reap_expired_slots, release_slot,
};
use assay_domain::Aeu;

use crate::lease_fakes::{Before, Call, MemoryRefs, lease_line, slot, slot_request};
use crate::ledger_fakes::{at, run};

const SLOT_0: &str = "refs/heads/slot-0";
const SLOT_1: &str = "refs/heads/slot-1";
const NOW: &str = "2026-10-12T09:30:00.000Z";

#[test]
fn a_free_slot_is_taken_by_creating_its_ref_at_the_lease() {
    let refs = MemoryRefs::default();

    let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");

    let SlotAcquired::Held(hold) = acquisition.acquired else {
        panic!("a free slot is taken: {acquisition:?}");
    };
    let (commit, message) = refs.get(SLOT_0).expect("slot-0 was created");
    assert_eq!(hold.slot, slot(0));
    assert_eq!(hold.lease_commit, commit);
    assert_eq!(message, lease_line(1, "2026-10-12T10:15:00.000Z"));
    assert_eq!(hold.lease.expires_at, at("2026-10-12T10:15:00.000Z"));
    assert_eq!(acquisition.reaped, []);
}

#[test]
fn a_second_acquirer_takes_the_other_slot_and_a_third_finds_both_busy() {
    let refs = MemoryRefs::default();

    let first = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");
    let second = acquire_slot(&refs, &slot_request(2), at(NOW)).expect("the store is up");
    let third = acquire_slot(&refs, &slot_request(3), at(NOW)).expect("the store is up");

    assert!(matches!(first.acquired, SlotAcquired::Held(ref hold) if hold.slot == slot(0)));
    assert!(matches!(second.acquired, SlotAcquired::Held(ref hold) if hold.slot == slot(1)));
    assert_eq!(third.acquired, SlotAcquired::Busy);
    assert_eq!(refs.names(), [SLOT_0, SLOT_1]);
}

#[test]
fn two_acquirers_racing_for_the_last_slot_exactly_one_wins() {
    let refs = MemoryRefs::default();
    refs.holding(SLOT_0, &lease_line(9, "2026-10-12T10:15:00.000Z"));
    let start = Barrier::new(2);

    let outcomes: Vec<SlotAcquired> = thread::scope(|scope| {
        let racers: Vec<_> = [1, 2]
            .map(|run_id| {
                let (refs, start) = (&refs, &start);
                thread::Builder::new()
                    .name(format!("acquirer-{run_id}"))
                    .spawn_scoped(scope, move || {
                        start.wait();
                        acquire_slot(refs, &slot_request(run_id), at(NOW))
                            .expect("the store is up")
                            .acquired
                    })
                    .expect("the test host starts threads")
            })
            .into();
        racers
            .into_iter()
            .map(|racer| racer.join().expect("no racer panics"))
            .collect()
    });

    let winners = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, SlotAcquired::Held(_)))
        .count();
    assert_eq!(winners, 1, "{outcomes:?}");
    assert!(outcomes.contains(&SlotAcquired::Busy), "{outcomes:?}");
}

#[test]
fn an_expired_lease_is_reaped_by_compare_and_set_and_the_slot_taken() {
    let refs = MemoryRefs::default();
    let stale = refs.holding(SLOT_0, &lease_line(9, "2026-10-12T09:29:59.999Z"));
    refs.holding(SLOT_1, &lease_line(8, "2026-10-12T10:00:00.000Z"));

    let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");

    assert!(
        matches!(acquisition.acquired, SlotAcquired::Held(ref hold) if hold.slot == slot(0)),
        "{acquisition:?}"
    );
    let [reaped] = acquisition.reaped.as_slice() else {
        panic!("one lease was reaped: {acquisition:?}");
    };
    assert_eq!((reaped.slot, &reaped.lease_commit), (slot(0), &stale));
    assert_eq!(reaped.lease.run_uid, run(9));
    assert_eq!(reaped.state, LeaseState::Expired);
    assert!(refs.calls().contains(&Call::Delete {
        name: SLOT_0.to_owned(),
        deleted: true
    }));
}

#[test]
fn a_lease_at_exactly_its_expiry_is_still_live() {
    let refs = MemoryRefs::default();
    let held = refs.holding(SLOT_0, &lease_line(9, NOW));
    refs.holding(SLOT_1, &lease_line(8, NOW));

    let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");

    assert_eq!(acquisition.acquired, SlotAcquired::Busy);
    assert_eq!(acquisition.reaped, []);
    assert_eq!(refs.get(SLOT_0).map(|(commit, _)| commit), Some(held));
    assert!(
        !refs
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Delete { .. })),
        "a live lease is never reaped"
    );
}

#[test]
fn a_slot_released_between_our_create_and_our_read_is_taken() {
    let refs = MemoryRefs::default().rival(Before::Read, |store| store.remove(SLOT_0));
    refs.holding(SLOT_0, &lease_line(9, "2026-10-12T10:15:00.000Z"));

    let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");

    assert!(
        matches!(acquisition.acquired, SlotAcquired::Held(ref hold) if hold.slot == slot(0)),
        "{acquisition:?}"
    );
}

#[test]
fn a_rival_reaping_first_leaves_us_on_the_next_slot() {
    // The rival reaps the expired slot-0 and takes it just before our delete.
    let refs = MemoryRefs::default().rival(Before::Delete, |store| {
        store.remove(SLOT_0);
        store.create(SLOT_0, &lease_line(5, "2026-10-12T10:15:00.000Z"));
    });
    refs.holding(SLOT_0, &lease_line(9, "2026-10-12T09:00:00.000Z"));

    let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");

    assert!(
        matches!(acquisition.acquired, SlotAcquired::Held(ref hold) if hold.slot == slot(1)),
        "{acquisition:?}"
    );
    assert_eq!(acquisition.reaped, [], "our compare-and-set reaped nothing");
    let (_, message) = refs.get(SLOT_0).expect("the rival holds slot-0");
    assert_eq!(message, lease_line(5, "2026-10-12T10:15:00.000Z"));
}

#[test]
fn release_deletes_the_lease_it_holds() {
    let refs = MemoryRefs::default();
    let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");
    let SlotAcquired::Held(hold) = acquisition.acquired else {
        panic!("a free slot is taken");
    };

    let released = release_slot(&refs, hold.slot, &hold.lease_commit).expect("the store is up");

    assert_eq!(released, Released::Released);
    assert_eq!(refs.names(), Vec::<String>::new());
}

#[test]
fn a_stale_holder_cannot_release_a_newer_holders_lease() {
    let refs = MemoryRefs::default();
    let stale = refs.holding(SLOT_0, &lease_line(9, "2026-10-12T09:00:00.000Z"));
    // A later admit reaps the expired lease and takes the slot.
    let newer = acquire_slot(&refs, &slot_request(1), at(NOW)).expect("the store is up");
    let SlotAcquired::Held(newer) = newer.acquired else {
        panic!("the expired slot is taken");
    };

    let released = release_slot(&refs, slot(0), &stale).expect("the store is up");

    assert_eq!(released, Released::NotHeld);
    assert_eq!(
        refs.get(SLOT_0).map(|(commit, _)| commit),
        Some(newer.lease_commit)
    );
}

#[test]
fn a_lease_assay_did_not_write_fails_the_acquisition_closed() {
    let canonical = lease_line(9, "2026-10-12T09:00:00.000Z");
    let refused = [
        canonical.replace(r#""pr":7"#, r#""pr":7,"extra":1"#),
        canonical.replace(r#","repo""#, r#", "repo""#),
        canonical.replace("ledger-lease/1", "ledger-lease/2"),
        canonical.replace("acme/widgets", "acme/.."),
        format!("{canonical}\n\n"),
        "x".repeat(MAX_LEASE_MESSAGE_BYTES + 1),
    ];
    for message in refused {
        let refs = MemoryRefs::default();
        refs.holding(SLOT_0, &message);

        let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW));

        let Err(LeaseError::Corrupt { name, problem }) = acquisition else {
            panic!("{message:?} must be refused: {acquisition:?}");
        };
        assert_eq!(name, LeaseRef::slot(slot(0)));
        assert_ne!(problem, Corruption::Duplicate, "{message:?}");
        assert!(
            !refs
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Delete { .. })),
            "an unreadable lease is never reaped: {message:?}"
        );
        assert_eq!(refs.names(), [SLOT_0], "nothing else is taken: {message:?}");
    }
}

#[test]
fn a_lease_ending_in_one_newline_reads_as_written() {
    let refs = MemoryRefs::default();
    refs.holding(
        SLOT_0,
        &format!("{}\n", lease_line(9, "2026-10-12T10:00:00.000Z")),
    );

    let views = read_slots(&refs, at(NOW)).expect("the lease reads");

    assert_eq!(views.len(), 1);
}

#[test]
fn read_slots_reports_live_and_expired_leases_with_their_reservations() {
    let refs = MemoryRefs::default();
    refs.holding(SLOT_0, &lease_line(9, "2026-10-12T09:00:00.000Z"));
    refs.holding(SLOT_1, &lease_line(8, "2026-10-12T10:00:00.000Z"));

    let views = read_slots(&refs, at(NOW)).expect("the store is up");

    let states: Vec<_> = views.iter().map(|view| (view.slot, view.state)).collect();
    assert_eq!(
        states,
        [(slot(0), LeaseState::Expired), (slot(1), LeaseState::Live)]
    );
    let reservations: Vec<LeaseReservation> =
        views.iter().map(assay_app::SlotView::reservation).collect();
    assert_eq!(
        reservations,
        [9, 8].map(|run_id| LeaseReservation {
            run_uid: run(run_id),
            reserved_aeu: Aeu::from_micros(2_300_000),
        })
    );
}

#[test]
fn reap_expired_slots_reaps_only_expired_leases() {
    let refs = MemoryRefs::default();
    refs.holding(SLOT_0, &lease_line(9, "2026-10-12T09:00:00.000Z"));
    let live = refs.holding(SLOT_1, &lease_line(8, "2026-10-12T10:00:00.000Z"));

    let reaped = reap_expired_slots(&refs, at(NOW)).expect("the store is up");

    let slots: Vec<Slot> = reaped.iter().map(|view| view.slot).collect();
    assert_eq!(slots, [slot(0)]);
    assert_eq!(refs.names(), [SLOT_1]);
    assert_eq!(refs.get(SLOT_1).map(|(commit, _)| commit), Some(live));
}

#[test]
fn a_store_that_cannot_be_reached_is_an_error() {
    let refs = MemoryRefs::default().down();

    let acquisition = acquire_slot(&refs, &slot_request(1), at(NOW));

    assert!(
        matches!(acquisition, Err(LeaseError::Refs(_))),
        "{acquisition:?}"
    );
}

#[test]
fn a_live_lease_outlasts_the_review_job() {
    assert_eq!(SLOT_TTL.as_secs(), 45 * 60);
    assert_eq!(MAX_REVIEW_TIMEOUT.as_secs(), 40 * 60);
    assert!(
        SLOT_TTL > MAX_REVIEW_TIMEOUT,
        "a live lease must never expire (R3-06)"
    );
}

#[test]
fn slots_are_named_by_index() {
    assert_eq!(
        Slot::ALL.map(|slot| LeaseRef::slot(slot).to_string()),
        [SLOT_0, SLOT_1]
    );
    assert_eq!(Slot::new(2), None);
    assert_eq!(slot(1).index(), 1);
}
