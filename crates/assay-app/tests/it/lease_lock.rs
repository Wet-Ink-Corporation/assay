//! The per-head publish lock `refs/heads/lock-<repo_id>-<head_sha>` (02 §5, R3-09): the slots'
//! primitive with a 2-minute TTL, reaped by the next writer once expired.

use assay_app::{
    Corruption, LOCK_TTL, LeaseError, LockAcquired, LockRequest, Released, acquire_lock,
    release_lock,
};

use crate::lease_fakes::{Call, MemoryRefs};
use crate::ledger_fakes::{at, commit, run};

const NOW: &str = "2026-10-12T09:30:00.000Z";

/// The lock of head `bbbb…` in repository 42.
fn lock_ref() -> String {
    format!("refs/heads/lock-42-{}", "b".repeat(40))
}

fn request(run_id: u64) -> LockRequest {
    LockRequest {
        run_uid: run(run_id),
        repo: "acme/widgets".to_owned(),
        head: commit('b'),
    }
}

/// The lock line of run `run_id` on head `head` (one repeated digit), expiring at `expires_at`.
fn lock_line(run_id: u64, head: char, expires_at: &str) -> String {
    format!(
        r#"{{"schema":"assay.ledger-lock/1","run_uid":{{"repo_id":42,"run_id":{run_id},"run_attempt":1}},"repo":"acme/widgets","head_sha":"{}","expires_at":"{expires_at}"}}"#,
        head.to_string().repeat(40)
    )
}

#[test]
fn a_free_lock_is_taken_by_creating_its_ref_with_a_two_minute_lease() {
    let refs = MemoryRefs::default();

    let acquired = acquire_lock(&refs, &request(1), at(NOW)).expect("the store is up");

    let LockAcquired::Held(hold) = acquired else {
        panic!("a free lock is taken: {acquired:?}");
    };
    let (commit, message) = refs.get(&lock_ref()).expect("the lock ref was created");
    assert_eq!(hold.lock_commit, commit);
    assert_eq!(message, lock_line(1, 'b', "2026-10-12T09:32:00.000Z"));
    assert_eq!(LOCK_TTL.as_secs(), 120);
}

#[test]
fn a_live_lock_is_busy() {
    let refs = MemoryRefs::default();
    refs.holding(&lock_ref(), &lock_line(9, 'b', "2026-10-12T09:31:00.000Z"));

    let acquired = acquire_lock(&refs, &request(1), at(NOW)).expect("the store is up");

    assert_eq!(acquired, LockAcquired::Busy);
}

#[test]
fn an_expired_lock_is_reaped_by_compare_and_set_and_taken() {
    let refs = MemoryRefs::default();
    refs.holding(&lock_ref(), &lock_line(9, 'b', "2026-10-12T09:29:00.000Z"));

    let acquired = acquire_lock(&refs, &request(1), at(NOW)).expect("the store is up");

    assert!(matches!(acquired, LockAcquired::Held(_)), "{acquired:?}");
    assert!(refs.calls().contains(&Call::Delete {
        name: lock_ref(),
        deleted: true
    }));
}

#[test]
fn release_deletes_only_the_lock_it_holds() {
    let refs = MemoryRefs::default();
    let LockAcquired::Held(stale) =
        acquire_lock(&refs, &request(1), at(NOW)).expect("the store is up")
    else {
        panic!("a free lock is taken");
    };
    // The lock expires; another writer reaps it and takes it.
    let LockAcquired::Held(newer) =
        acquire_lock(&refs, &request(2), at("2026-10-12T09:33:00.000Z")).expect("the store is up")
    else {
        panic!("an expired lock is taken");
    };

    let stale_release = release_lock(&refs, &stale).expect("the store is up");
    let newer_release = release_lock(&refs, &newer).expect("the store is up");

    assert_eq!(
        (stale_release, newer_release),
        (Released::NotHeld, Released::Released)
    );
    assert_eq!(refs.names(), Vec::<String>::new());
}

#[test]
fn a_lock_lease_naming_another_head_is_refused() {
    let refs = MemoryRefs::default();
    refs.holding(&lock_ref(), &lock_line(9, 'c', "2026-10-12T09:00:00.000Z"));

    let acquired = acquire_lock(&refs, &request(1), at(NOW));

    assert!(
        matches!(
            acquired,
            Err(LeaseError::Corrupt {
                problem: Corruption::WrongRef,
                ..
            })
        ),
        "{acquired:?}"
    );
    assert_eq!(
        refs.get(&lock_ref()).map(|(_, message)| message),
        Some(lock_line(9, 'c', "2026-10-12T09:00:00.000Z")),
        "a lock bound to another ref is never reaped"
    );
}
