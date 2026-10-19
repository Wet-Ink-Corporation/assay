//! The single ledger-side reconcile (06 §4, §1, F17, F21, R3-06, R3-16): reaps expired slot
//! leases and publish locks by compare-and-set, and back-fills the `final` cost record of runs
//! whose `release` never wrote one, deduplicated by `run_uid`.

use assay_app::{
    CostKind, LedgerPath, LedgerReconcileError, LedgerReconcileRequest, MAX_PUBLISH_LOCKS,
    reconcile_ledger,
};
use assay_domain::{Aeu, RunUid};

use crate::lease_fakes::{Before, Call, MemoryRefs, RefsDown, lease_line, slot};
use crate::ledger_fakes::{Down, MemoryLedger, at, cost, cost_line, id, run};

const NOW: &str = "2026-10-12T12:00:00.000Z";

fn request() -> LedgerReconcileRequest {
    LedgerReconcileRequest {
        run_uid: RunUid {
            repo_id: id(900),
            run_id: id(5000),
            run_attempt: id(1),
        },
        now: at(NOW),
    }
}

fn slot_ref(index: u8) -> String {
    format!("refs/heads/slot-{index}")
}

fn october() -> LedgerPath {
    LedgerPath::costs(at(NOW).month())
}

/// The lock of head `head` (one repeated digit) in repository 42.
fn lock_ref(head: char) -> String {
    format!("refs/heads/lock-42-{}", head.to_string().repeat(40))
}

/// The lock line of run `run_id` on head `head`, expiring at `expires_at`.
fn lock_line(run_id: u64, head: char, expires_at: &str) -> String {
    format!(
        r#"{{"schema":"assay.ledger-lock/1","run_uid":{{"repo_id":42,"run_id":{run_id},"run_attempt":1}},"repo":"acme/widgets","head_sha":"{}","expires_at":"{expires_at}"}}"#,
        head.to_string().repeat(40)
    )
}

/// The `final` back-fill line of `cost_line(run_id, "reserved", aeu, _)`, written at `NOW`.
fn back_fill_line(run_id: u64, aeu: &str) -> String {
    cost_line(run_id, "final", aeu, NOW).replace(r#""cancelled":false"#, r#""cancelled":true"#)
}

#[test]
fn a_re_run_is_refused_before_any_port_is_called() {
    let ledger = MemoryLedger::default().down();
    let refs = MemoryRefs::default().down();
    let mut request = request();
    request.run_uid.run_attempt = id(2);

    let refused = reconcile_ledger(&ledger, &refs, &request);

    assert!(
        matches!(refused, Err(LedgerReconcileError::NotFirstAttempt { attempt }) if attempt == id(2)),
        "{refused:?}"
    );
    assert_eq!(refs.calls(), []);
}

#[test]
fn an_expired_slot_lease_is_reaped_and_a_live_one_is_kept() {
    let ledger = MemoryLedger::default();
    let refs = MemoryRefs::default();
    refs.holding(&slot_ref(0), &lease_line(1, "2026-10-12T11:59:59.999Z"));
    let live = refs.holding(&slot_ref(1), &lease_line(2, NOW));

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    let reaped: Vec<_> = reconciled
        .reaped_slots
        .iter()
        .map(|view| view.slot)
        .collect();
    assert_eq!(reaped, [slot(0)]);
    assert_eq!(refs.get(&slot_ref(0)), None);
    assert_eq!(refs.get(&slot_ref(1)).map(|(commit, _)| commit), Some(live));
}

#[test]
fn a_slot_taken_again_after_the_read_is_not_deleted() {
    let ledger = MemoryLedger::default();
    let retaken = lease_line(3, "2026-10-12T12:45:00.000Z");
    let refs = MemoryRefs::default().rival(Before::Delete, move |store| {
        store.remove("refs/heads/slot-0");
        store.create("refs/heads/slot-0", &retaken);
    });
    refs.holding(&slot_ref(0), &lease_line(1, "2026-10-12T11:00:00.000Z"));

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    assert_eq!(reconciled.reaped_slots, []);
    let (_, message) = refs
        .get(&slot_ref(0))
        .expect("the new holder keeps the slot");
    assert_eq!(message, lease_line(3, "2026-10-12T12:45:00.000Z"));
}

#[test]
fn an_unsettled_reservation_whose_lease_is_gone_is_back_filled_at_its_reservation() {
    let reserved = cost_line(1, "reserved", "0.74", "2026-10-12T10:00:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), reserved.as_bytes());
    let refs = MemoryRefs::default();

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    let mut expected = cost(1, CostKind::Final, 740_000, NOW);
    expected.cancelled = true;
    assert_eq!(reconciled.back_filled, [expected]);
    assert_eq!(
        ledger.text(&october()),
        format!("{reserved}{}", back_fill_line(1, "0.74"))
    );
}

#[test]
fn a_reaped_lease_whose_reservation_is_unsettled_is_back_filled_in_the_same_sweep() {
    let reserved = cost_line(1, "reserved", "2.3", "2026-10-12T10:00:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), reserved.as_bytes());
    let refs = MemoryRefs::default();
    refs.holding(&slot_ref(0), &lease_line(1, "2026-10-12T10:45:00.000Z"));

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    assert_eq!(reconciled.reaped_slots.len(), 1);
    let runs: Vec<_> = reconciled
        .back_filled
        .iter()
        .map(|record| record.run_uid)
        .collect();
    assert_eq!(runs, [run(1)]);
    assert_eq!(reconciled.unrecorded, []);
}

#[test]
fn settled_live_and_young_reservations_are_not_back_filled() {
    let file = [
        // Settled by its release.
        cost_line(1, "reserved", "0.74", "2026-10-12T10:00:00.000Z"),
        cost_line(1, "final", "0.31", "2026-10-12T10:30:00.000Z"),
        // Its slot is still held by a live lease.
        cost_line(2, "reserved", "2.3", "2026-10-12T10:00:00.000Z"),
        // Reserved less than the slot TTL ago.
        cost_line(3, "reserved", "0.74", "2026-10-12T11:15:00.000Z"),
    ]
    .concat();
    let ledger = MemoryLedger::default().with_file(&october(), file.as_bytes());
    let refs = MemoryRefs::default();
    refs.holding(&slot_ref(1), &lease_line(2, "2026-10-12T12:10:00.000Z"));

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    assert_eq!(reconciled.back_filled, []);
    assert_eq!(ledger.writes(), []);
}

#[test]
fn a_reservation_exactly_one_ttl_old_is_not_yet_back_filled() {
    let reserved = cost_line(1, "reserved", "0.74", "2026-10-12T11:15:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), reserved.as_bytes());
    let refs = MemoryRefs::default();
    let mut request = request();
    request.now = at("2026-10-12T12:00:00.000Z");

    let reconciled = reconcile_ledger(&ledger, &refs, &request).expect("the ledger is up");

    assert_eq!(reconciled.back_filled, []);
}

#[test]
fn a_second_sweep_back_fills_nothing() {
    let reserved = cost_line(1, "reserved", "0.74", "2026-10-12T10:00:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), reserved.as_bytes());
    let refs = MemoryRefs::default();
    reconcile_ledger(&ledger, &refs, &request()).expect("the first sweep runs");

    let again = reconcile_ledger(&ledger, &refs, &request()).expect("the second sweep runs");

    assert_eq!(again.back_filled, []);
    assert_eq!(
        ledger.text(&october()),
        format!("{reserved}{}", back_fill_line(1, "0.74"))
    );
}

#[test]
fn a_release_landing_during_the_sweep_wins_and_nothing_is_written() {
    let reserved = cost_line(1, "reserved", "0.74", "2026-10-12T10:00:00.000Z");
    let release = cost_line(1, "final", "0.31", "2026-10-12T11:59:00.000Z");
    let ledger = MemoryLedger::default()
        .with_file(&october(), reserved.as_bytes())
        .rival_appends(release.as_bytes());
    let refs = MemoryRefs::default();

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    assert_eq!(reconciled.back_filled, []);
    assert_eq!(reconciled.already_settled, [run(1)]);
    assert_eq!(ledger.text(&october()), format!("{reserved}{release}"));
}

#[test]
fn a_reaped_lease_with_no_cost_record_is_reported_unrecorded() {
    let ledger = MemoryLedger::default();
    let refs = MemoryRefs::default();
    refs.holding(&slot_ref(0), &lease_line(1, "2026-10-12T10:45:00.000Z"));

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    let unrecorded: Vec<_> = reconciled
        .unrecorded
        .iter()
        .map(|view| view.lease.run_uid)
        .collect();
    assert_eq!(unrecorded, [run(1)]);
    assert_eq!(
        reconciled
            .unrecorded
            .first()
            .map(|view| view.lease.reserved_aeu),
        Some(Aeu::from_micros(2_300_000))
    );
    assert_eq!(reconciled.back_filled, []);
}

#[test]
fn an_expired_publish_lock_is_reaped_and_a_live_one_is_kept() {
    let ledger = MemoryLedger::default();
    let refs = MemoryRefs::default();
    refs.holding(
        &lock_ref('b'),
        &lock_line(1, 'b', "2026-10-12T11:58:00.000Z"),
    );
    refs.holding(
        &lock_ref('c'),
        &lock_line(2, 'c', "2026-10-12T12:01:00.000Z"),
    );

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    let reaped: Vec<_> = reconciled
        .reaped_locks
        .iter()
        .map(|lease| lease.run_uid)
        .collect();
    assert_eq!(reaped, [run(1)]);
    assert_eq!(refs.names(), [lock_ref('c')]);
}

#[test]
fn a_lock_taken_again_after_the_read_is_not_deleted() {
    let ledger = MemoryLedger::default();
    let name = lock_ref('b');
    let retaken = lock_line(3, 'b', "2026-10-12T12:02:00.000Z");
    let rival_name = name.clone();
    let rival_line = retaken.clone();
    let refs = MemoryRefs::default().rival(Before::Delete, move |store| {
        store.remove(&rival_name);
        store.create(&rival_name, &rival_line);
    });
    refs.holding(&name, &lock_line(1, 'b', "2026-10-12T11:58:00.000Z"));

    let reconciled = reconcile_ledger(&ledger, &refs, &request()).expect("the ledger is up");

    assert_eq!(reconciled.reaped_locks, []);
    assert_eq!(refs.get(&name).map(|(_, message)| message), Some(retaken));
    assert!(
        refs.calls().contains(&Call::Delete {
            name,
            deleted: false
        }),
        "the reap was a compare-and-set that found the ref moved"
    );
}

/// A ledger with an unsettled reservation past its TTL, and refs with its expired slot lease:
/// work a sweep would do, to show that a refusal does none of it.
fn work_pending() -> (MemoryLedger, MemoryRefs) {
    let reserved = cost_line(1, "reserved", "2.3", "2026-10-12T10:00:00.000Z");
    let ledger = MemoryLedger::default().with_file(&october(), reserved.as_bytes());
    let refs = MemoryRefs::default();
    refs.holding(&slot_ref(0), &lease_line(1, "2026-10-12T10:45:00.000Z"));
    (ledger, refs)
}

#[test]
fn a_lock_ref_assay_did_not_name_refuses_the_sweep_before_any_reap_or_back_fill() {
    let (ledger, refs) = work_pending();
    refs.holding("refs/heads/lock-042-x", "anything");

    let refused = reconcile_ledger(&ledger, &refs, &request());

    assert!(
        matches!(&refused, Err(LedgerReconcileError::LockName { name }) if name == "refs/heads/lock-042-x"),
        "{refused:?}"
    );
    assert_eq!(refs.names(), ["refs/heads/lock-042-x", &slot_ref(0)]);
    assert_eq!(ledger.writes(), []);
}

#[test]
fn a_refused_lock_name_is_shown_escaped_and_capped() {
    let name = format!("refs/heads/lock-\n\u{1b}[31m{}", "x".repeat(400));
    let refused: LedgerReconcileError<Down, RefsDown> = LedgerReconcileError::LockName { name };

    let shown = refused.to_string();

    assert!(!shown.contains(['\n', '\u{1b}']), "{shown}");
    assert!(shown.contains(r"lock-\n\u{1b}[31m"), "{shown}");
    assert!(shown.len() < 300, "{} bytes: {shown}", shown.len());
}

#[test]
fn a_lock_naming_another_head_is_refused_and_kept() {
    let ledger = MemoryLedger::default();
    let refs = MemoryRefs::default();
    refs.holding(
        &lock_ref('b'),
        &lock_line(1, 'c', "2026-10-12T11:58:00.000Z"),
    );

    let refused = reconcile_ledger(&ledger, &refs, &request());

    assert!(
        matches!(refused, Err(LedgerReconcileError::Locks(_))),
        "{refused:?}"
    );
    assert_eq!(refs.names(), [lock_ref('b')]);
}

#[test]
fn more_publish_locks_than_the_cap_refuse_the_sweep_before_any_reap_or_back_fill() {
    let (ledger, refs) = work_pending();
    for n in 0..=MAX_PUBLISH_LOCKS {
        let head = format!("{n:040x}");
        refs.holding(&format!("refs/heads/lock-42-{head}"), "unread");
    }

    let refused = reconcile_ledger(&ledger, &refs, &request());

    assert!(
        matches!(
            refused,
            Err(LedgerReconcileError::TooManyLocks { limit }) if limit == MAX_PUBLISH_LOCKS
        ),
        "{refused:?}"
    );
    assert!(
        refs.names().contains(&slot_ref(0)),
        "the slot was not reaped"
    );
    assert_eq!(ledger.writes(), []);
}

#[test]
fn an_unreadable_slot_lease_fails_the_sweep_and_is_kept() {
    let ledger = MemoryLedger::default();
    let refs = MemoryRefs::default();
    refs.holding(&slot_ref(0), "not a lease");

    let refused = reconcile_ledger(&ledger, &refs, &request());

    assert!(
        matches!(refused, Err(LedgerReconcileError::Slots(_))),
        "{refused:?}"
    );
    assert_eq!(refs.names(), [slot_ref(0)]);
}
