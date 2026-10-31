//! The `release` use case (S10, 04 §1, 06 §1, R3-06, R3-16): releases the review slot by
//! compare-and-set, then appends the run's `final` cost record, at the cost the review reported
//! or, for a review that died or was cancelled, at the run cap its reservation holds.

use assay_app::{
    Appended, CommitId, CostKind, LeaseError, LedgerPath, ReleaseError, ReleasePorts,
    ReleaseRequest, Released, ReviewCost, release,
};
use assay_domain::Aeu;

use crate::lease_fakes::{Before, Call, MemoryRefs, lease_line, slot};
use crate::ledger_fakes::{MemoryLedger, at, commit, cost, cost_line, id, run};

const RESERVED_AT: &str = "2026-10-12T09:30:00.000Z";
const NOW: &str = "2026-10-12T09:55:00.000Z";
const SLOT_REF: &str = "refs/heads/slot-1";

fn october() -> LedgerPath {
    LedgerPath::costs(at(NOW).month())
}

/// The ledger after `admit` reserved 0.74 for run 1, and the refs with its lease on slot 1.
fn admitted() -> (MemoryLedger, MemoryRefs, CommitId) {
    let reserved = cost_line(1, "reserved", "0.74", RESERVED_AT);
    let ledger = MemoryLedger::default().with_file(&october(), reserved.as_bytes());
    let refs = MemoryRefs::default();
    let lease = refs.holding(SLOT_REF, &lease_line(1, "2026-10-12T10:15:00.000Z"));
    (ledger, refs, lease)
}

fn request(lease: &CommitId, review: ReviewCost) -> ReleaseRequest<'_> {
    ReleaseRequest {
        run_uid: run(1),
        slot: slot(1),
        lease_commit: lease,
        review,
        now: at(NOW),
    }
}

/// The `final` line of run 1 at `aeu`, with `roles` its roles' JSON and `cancelled` its flag.
fn final_line(aeu: &str, roles: &str, cancelled: bool) -> String {
    cost_line(1, "final", aeu, NOW)
        .replace(
            &format!(r#""roles":[{{"role":"combined","aeu":"{aeu}"}}]"#),
            roles,
        )
        .replace(
            r#""cancelled":false"#,
            &format!(r#""cancelled":{cancelled}"#),
        )
}

#[test]
fn a_spent_review_releases_its_slot_then_records_the_reported_cost() {
    let (ledger, refs, lease) = admitted();
    let reserved = ledger.text(&october());

    let settled = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Reported(Aeu::from_micros(512_000))),
    )
    .expect("the ledger and refs are up");

    assert_eq!(settled.slot, Released::Released);
    assert_eq!(settled.cost, Appended::Recorded);
    assert_eq!(refs.get(SLOT_REF), None);
    assert_eq!(
        ledger.text(&october()),
        reserved + &final_line("0.512", r#""roles":[]"#, false)
    );
}

#[test]
fn a_failed_review_is_charged_the_run_cap_and_is_not_marked_cancelled() {
    let (ledger, refs, lease) = admitted();
    let reserved = ledger.text(&october());

    let settled = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Died),
    )
    .expect("the ledger and refs are up");

    assert_eq!(settled.record, cost(1, CostKind::Final, 740_000, NOW));
    assert_eq!(
        ledger.text(&october()),
        reserved + &cost_line(1, "final", "0.74", NOW)
    );
}

#[test]
fn a_cancelled_review_is_charged_the_run_cap_and_marked_cancelled() {
    let (ledger, refs, lease) = admitted();
    let reserved = ledger.text(&october());

    let settled = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Cancelled),
    )
    .expect("the ledger and refs are up");

    let mut charged = cost(1, CostKind::Final, 740_000, NOW);
    charged.cancelled = true;
    assert_eq!(settled.record, charged);
    assert_eq!(
        ledger.text(&october()),
        reserved
            + &final_line(
                "0.74",
                r#""roles":[{"role":"combined","aeu":"0.74"}]"#,
                true
            )
    );
}

#[test]
fn a_slot_reaped_since_is_reported_and_the_final_record_is_still_written() {
    let (ledger, refs, lease) = admitted();
    let refs = refs.rival(Before::Delete, |store| {
        store.remove(SLOT_REF);
        store.create(SLOT_REF, &lease_line(2, "2026-10-12T10:40:00.000Z"));
    });
    let reserved = ledger.text(&october());

    let settled = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Reported(Aeu::ZERO)),
    )
    .expect("the ledger and refs are up");

    assert_eq!(
        (settled.slot, settled.cost),
        (Released::NotHeld, Appended::Recorded)
    );
    assert!(
        refs.get(SLOT_REF)
            .is_some_and(|(commit, _)| commit != lease),
        "the newer lease is kept"
    );
    assert_eq!(
        ledger.text(&october()),
        reserved + &final_line("0", r#""roles":[]"#, false)
    );
}

#[test]
fn a_final_record_already_there_is_reported_and_nothing_is_written() {
    let settled_before = cost_line(1, "reserved", "0.74", RESERVED_AT)
        + &cost_line(1, "final", "0.3", "2026-10-12T09:50:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), settled_before.as_bytes());
    let refs = MemoryRefs::default();
    let lease = refs.holding(SLOT_REF, &lease_line(1, "2026-10-12T10:15:00.000Z"));

    let settled = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Reported(Aeu::from_micros(512_000))),
    )
    .expect("the ledger and refs are up");

    assert_eq!(
        settled.cost,
        Appended::AlreadyRecorded(cost(
            1,
            CostKind::Final,
            300_000,
            "2026-10-12T09:50:00.000Z"
        ))
    );
    assert_eq!(ledger.writes(), []);
    assert_eq!(ledger.text(&october()), settled_before);
}

#[test]
fn a_run_with_no_reservation_releases_nothing_and_writes_nothing() {
    let other = cost_line(2, "reserved", "0.74", RESERVED_AT);
    let ledger = MemoryLedger::default().with_file(&october(), other.as_bytes());
    let refs = MemoryRefs::default();
    let lease = refs.holding(SLOT_REF, &lease_line(1, "2026-10-12T10:15:00.000Z"));

    let refused = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Died),
    );

    assert!(
        matches!(refused, Err(ReleaseError::NoReservation)),
        "{refused:?}"
    );
    assert_eq!(refs.calls(), []);
    assert_eq!(ledger.writes(), []);
}

#[test]
fn a_re_run_is_refused_before_any_port_is_called() {
    let ledger = MemoryLedger::default().down();
    let refs = MemoryRefs::default().down();
    let lease = commit('c');
    let mut request = request(&lease, ReviewCost::Died);
    request.run_uid.run_attempt = id(2);

    let refused = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request,
    );

    assert!(
        matches!(refused, Err(ReleaseError::NotFirstAttempt { attempt }) if attempt == id(2)),
        "{refused:?}"
    );
    assert_eq!(refs.calls(), []);
}

#[test]
fn a_slot_that_cannot_be_released_writes_no_record() {
    let reserved = cost_line(1, "reserved", "0.74", RESERVED_AT);
    let ledger = MemoryLedger::default().with_file(&october(), reserved.as_bytes());
    let refs = MemoryRefs::default().down();
    let lease = commit('c');

    let refused = release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Reported(Aeu::ZERO)),
    );

    assert!(
        matches!(
            refused,
            Err(ReleaseError::Slot { slot: found, source: LeaseError::Refs(_) }) if found == slot(1)
        ),
        "{refused:?}"
    );
    assert_eq!(ledger.writes(), []);
}

#[test]
fn the_slot_is_released_by_compare_and_set_on_the_lease_commit() {
    let (ledger, refs, lease) = admitted();

    release(
        &ReleasePorts {
            ledger: &ledger,
            refs: &refs,
        },
        &request(&lease, ReviewCost::Reported(Aeu::ZERO)),
    )
    .expect("the ledger and refs are up");

    assert_eq!(
        refs.calls(),
        [Call::Delete {
            name: SLOT_REF.to_owned(),
            deleted: true,
        }]
    );
}
