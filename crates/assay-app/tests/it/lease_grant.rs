//! The one-shot retry grants `refs/heads/retry/<repo_id>/<pr>/<head_sha>/<role>` (06 §1, R3-14):
//! created atomically by decide-only, consumed by `admit` by compare-and-set.

use assay_app::{
    Consumed, Corruption, GrantKey, Granted, LeaseError, RetryGrant, Role, consume_grant,
    grant_retry,
};

use crate::lease_fakes::{Before, MemoryRefs};
use crate::ledger_fakes::{at, commit, id, run};

/// The grant ref of `security` on head `bbbb…` of PR 7 in repository 42.
fn grant_ref() -> String {
    format!("refs/heads/retry/42/7/{}/security", "b".repeat(40))
}

fn grant(run_id: u64) -> RetryGrant {
    RetryGrant {
        run_uid: run(run_id),
        repo: "acme/widgets".to_owned(),
        pr: id(7),
        head_sha: commit('b'),
        role: Role::Security,
        at: at("2026-10-12T09:30:00.000Z"),
    }
}

fn key() -> GrantKey {
    GrantKey {
        repo_id: id(42),
        pr: id(7),
        head_sha: commit('b'),
        role: Role::Security,
    }
}

/// The grant line of run `run_id` for `role`.
fn grant_line(run_id: u64, role: &str) -> String {
    format!(
        r#"{{"schema":"assay.ledger-grant/1","run_uid":{{"repo_id":42,"run_id":{run_id},"run_attempt":1}},"repo":"acme/widgets","pr":7,"head_sha":"{}","role":"{role}","at":"2026-10-12T09:30:00.000Z"}}"#,
        "b".repeat(40)
    )
}

#[test]
fn a_grant_is_created_once_per_head_and_role() {
    let refs = MemoryRefs::default();

    let first = grant_retry(&refs, &grant(1)).expect("the store is up");
    let second = grant_retry(&refs, &grant(2)).expect("the store is up");

    assert_eq!((first, second), (Granted::Granted, Granted::AlreadyGranted));
    assert_eq!(
        refs.get(&grant_ref()).map(|(_, message)| message),
        Some(grant_line(1, "security"))
    );
}

#[test]
fn a_grant_is_consumed_once() {
    let refs = MemoryRefs::default();
    grant_retry(&refs, &grant(1)).expect("the store is up");

    let first = consume_grant(&refs, &key()).expect("the store is up");
    let second = consume_grant(&refs, &key()).expect("the store is up");

    assert_eq!(
        (first, second),
        (Consumed::Consumed(grant(1)), Consumed::NoGrant)
    );
    assert_eq!(refs.names(), Vec::<String>::new());
}

#[test]
fn a_grant_another_admit_consumed_first_is_not_consumed_twice() {
    let refs = MemoryRefs::default().rival(Before::Delete, |store| store.remove(&grant_ref()));
    grant_retry(&refs, &grant(1)).expect("the store is up");

    let consumed = consume_grant(&refs, &key()).expect("the store is up");

    assert_eq!(consumed, Consumed::NoGrant);
}

#[test]
fn no_grant_is_no_grant() {
    let refs = MemoryRefs::default();

    let consumed = consume_grant(&refs, &key()).expect("the store is up");

    assert_eq!(consumed, Consumed::NoGrant);
}

#[test]
fn a_grant_naming_another_role_is_refused_and_kept() {
    let refs = MemoryRefs::default();
    refs.holding(&grant_ref(), &grant_line(1, "bugs"));

    let consumed = consume_grant(&refs, &key());

    assert!(
        matches!(
            consumed,
            Err(LeaseError::Corrupt {
                problem: Corruption::WrongRef,
                ..
            })
        ),
        "{consumed:?}"
    );
    assert_eq!(refs.names(), [grant_ref()]);
}

#[test]
fn a_grant_of_an_unknown_role_is_refused() {
    let refs = MemoryRefs::default();
    refs.holding(&grant_ref(), &grant_line(1, "verifier"));

    let consumed = consume_grant(&refs, &key());

    assert!(
        matches!(
            consumed,
            Err(LeaseError::Corrupt {
                problem: Corruption::Field("role"),
                ..
            })
        ),
        "{consumed:?}"
    );
}

#[test]
fn a_grant_its_reader_would_refuse_writes_nothing() {
    let refs = MemoryRefs::default();
    let grant = RetryGrant {
        repo: "acme/..".to_owned(),
        ..grant(1)
    };

    let granted = grant_retry(&refs, &grant);

    assert!(
        matches!(
            granted,
            Err(LeaseError::Invalid {
                problem: Corruption::Field("repo")
            })
        ),
        "{granted:?}"
    );
    assert_eq!(refs.calls(), []);
}
