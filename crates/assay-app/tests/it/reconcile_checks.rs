//! The per-repo reconcile (06 §4, 02 §5, R3-06, R3-16): a consumer repository closes its own
//! stuck verdict checks as `failure (publish-error)` and its superseded heads' as `cancelled`,
//! each under the head's publish lock and by compare-and-set on the check's `external_id`.

use std::rc::Rc;

use assay_app::{
    CheckConclusion, CheckRunId, CheckState, CheckStatus, ChecksReconcileError,
    ChecksReconcileRequest, ChecksReconciled, ClosedCheck, Closure, CommitId, Deadline, LedgerPath,
    Listing, LostLock, MAX_HEAD_CHECKS, MAX_OPEN_PULLS, MAX_SUPERSEDED_HEADS, PullHead,
    ReconcilePorts, RunProgress, Skip, SkippedCheck, reconcile_checks,
};
use assay_domain::RunUid;

use crate::lease_fakes::{Before, Call, MemoryRefs, RefsDown, lease_line};
use crate::ledger_fakes::{Down, MemoryLedger, at, commit, id};
use crate::reconcile_fakes::{FakeRepo, GitHubDown, NoDelete, check, head};

const NOW: &str = "2026-10-12T12:00:00.000Z";
/// A run started 50 minutes and a millisecond before `NOW`: its check is stuck.
const OLD: &str = "2026-10-12T11:09:59.999Z";
/// A run started exactly 50 minutes before `NOW`: its check is not yet stuck.
const DUE: &str = "2026-10-12T11:10:00.000Z";
const VERDICT: &str = "assay / verdict";
/// The stamp `open` wrote for run 100.
const STAMP: &str = "pending:100:1";

fn request() -> ChecksReconcileRequest<'static> {
    ChecksReconcileRequest {
        run_uid: RunUid {
            repo_id: id(42),
            run_id: id(5000),
            run_attempt: id(1),
        },
        repo: "acme/widgets",
        now: at(NOW),
        max_requeues: 3,
    }
}

type Swept = Result<ChecksReconciled, ChecksReconcileError<GitHubDown, Down, RefsDown>>;

fn sweep(forge: &FakeRepo, ledger: &MemoryLedger, refs: &MemoryRefs) -> Swept {
    reconcile_checks(
        ReconcilePorts {
            forge,
            ledger,
            refs,
        },
        &request(),
    )
}

/// The verdict check 1, in progress, stamped by run 100.
fn pending() -> assay_app::ReconcileCheck {
    check(1, VERDICT, STAMP)
}

fn check_id(n: u64) -> CheckRunId {
    CheckRunId(id(n))
}

/// A repository with PR 7 open on head `b…`.
fn on_b() -> FakeRepo {
    FakeRepo::default().pull(id(7), commit('b'))
}

/// PR 7 on head `b…` with its verdict check stamped by run 100, which completed after starting
/// at `OLD`: stuck.
fn stuck() -> FakeRepo {
    on_b()
        .check(commit('b'), pending())
        .run(100, RunProgress::Completed, at(OLD))
}

/// PR 7's push records for the pushes that moved its head through `heads`, in order.
fn pushes(heads: &[CommitId]) -> String {
    heads
        .iter()
        .zip(heads.iter().skip(1))
        .zip(1_u64..)
        .map(|((before, after), seq)| {
            format!(
                concat!(
                    r#"{{"schema":"assay.ledger-push/1","run_uid":{{"repo_id":42,"run_id":{run},"#,
                    r#""run_attempt":1}},"pr":7,"seq":{seq},"kind":"push","before":"{before}","after":"{after}","#,
                    r#""sender_id":1001,"forced":false,"at":"2026-10-12T09:{seq:02}:00.000Z"}}"#,
                    "\n"
                ),
                run = 200 + seq,
                seq = seq,
                before = before,
                after = after,
            )
        })
        .collect::<Vec<_>>()
        .concat()
}

/// A ledger holding PR 7's push records through `heads`.
fn ledger_with_pushes(heads: &[CommitId]) -> MemoryLedger {
    MemoryLedger::default().with_file(&LedgerPath::pushes(id(42), id(7)), pushes(heads).as_bytes())
}

/// PR 7's state with a deferral for head `head` requeued `count` times.
fn deferred(head: char, count: u32) -> MemoryLedger {
    let state = format!(
        concat!(
            r#"{{"schema":"assay.ledger-state/1","repo_id":42,"pr":7,"evidence_records":[],"#,
            r#""deferral":{{"repo":"acme/widgets","pr":7,"head":"{head}","count":{count},"#,
            r#""flag":false}},"actor_counters":[]}}"#,
            "\n"
        ),
        head = head.to_string().repeat(40),
        count = count,
    );
    MemoryLedger::default().with_file(&LedgerPath::state(id(42), id(7)), state.as_bytes())
}

fn lock_ref(head: char) -> String {
    format!("refs/heads/lock-42-{}", head.to_string().repeat(40))
}

fn skipped(head: char, reason: Skip) -> SkippedCheck {
    SkippedCheck {
        pr: id(7),
        head: commit(head),
        check: check_id(1),
        reason,
    }
}

#[test]
fn a_re_run_is_refused_before_any_port_is_called() {
    let forge = on_b().down();
    let ledger = MemoryLedger::default().down();
    let refs = MemoryRefs::default().down();
    let mut request = request();
    request.run_uid.run_attempt = id(2);

    let refused = reconcile_checks(
        ReconcilePorts {
            forge: &forge,
            ledger: &ledger,
            refs: &refs,
        },
        &request,
    );

    assert!(
        matches!(refused, Err(ChecksReconcileError::NotFirstAttempt { attempt }) if attempt == id(2)),
        "{refused:?}"
    );
}

#[test]
fn a_stuck_verdict_check_is_closed_as_a_publish_error_while_its_lock_is_held() {
    let refs = Rc::new(MemoryRefs::default());
    let watched = Rc::clone(&refs);
    let forge = stuck().probing(move || watched.names().contains(&lock_ref('b')));

    let done = sweep(&forge, &MemoryLedger::default(), &refs).expect("the ports are up");

    assert_eq!(
        done.closed,
        [ClosedCheck {
            pr: id(7),
            head: commit('b'),
            check: check_id(1),
            closure: Closure::Stuck,
        }]
    );
    assert_eq!(
        forge.probed(),
        [true],
        "the head's lock was held at the write"
    );
    let writes = forge.writes();
    let [write] = writes.as_slice() else {
        panic!("one write: {writes:?}");
    };
    assert_eq!(write.expected, STAMP);
    assert_eq!(
        write.update.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    assert_eq!(write.update.title, "assay: failure (publish-error)");
    assert_eq!(write.update.text, "");
    assert_eq!(write.update.external_id, "reconcile:5000");
    assert_eq!(refs.names(), Vec::<String>::new(), "and released after it");
}

#[test]
fn a_close_is_bounded_by_its_locks_lease_expiry() {
    // The lock is taken at `NOW` for two minutes; the write may not outlast it (#290).
    let refs = MemoryRefs::default();
    let forge = stuck();

    sweep(&forge, &MemoryLedger::default(), &refs).expect("the ports are up");

    let deadlines: Vec<_> = forge.writes().iter().map(|write| write.deadline).collect();
    assert_eq!(deadlines, [Deadline::at(at("2026-10-12T12:02:00.000Z"))]);
}

#[test]
fn checks_that_are_not_stuck_are_left() {
    let cases: [(&str, FakeRepo); 4] = [
        (
            "its run started 50 minutes ago",
            on_b()
                .check(commit('b'), pending())
                .run(100, RunProgress::Completed, at(DUE)),
        ),
        (
            "its run is live",
            on_b()
                .check(commit('b'), pending())
                .run(100, RunProgress::Live, at(OLD)),
        ),
        (
            "a gate check",
            on_b()
                .check(commit('b'), check(1, "assay / gate: temper", STAMP))
                .run(100, RunProgress::Completed, at(OLD)),
        ),
        ("a completed check", {
            let mut done = pending();
            done.status = CheckStatus::Completed;
            on_b()
                .check(commit('b'), done)
                .run(100, RunProgress::Completed, at(OLD))
        }),
    ];
    for (case, forge) in cases {
        let done = sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default())
            .expect("the ports are up");

        assert_eq!(done, ChecksReconciled::default(), "{case}");
        assert_eq!(forge.writes(), [], "{case}");
    }
}

/// A requeue (`workflow_dispatch`) or decide-only run runs on the default branch's commit, not
/// the PR's head: the check is live because the run its stamp names is, wherever that run is.
#[test]
fn a_check_whose_stamped_run_is_live_on_another_commit_is_left() {
    let forge = on_b()
        .check(commit('b'), check(1, VERDICT, "pending:39282503375:2"))
        .run(39_282_503_375, RunProgress::Live, at(OLD));

    let done =
        sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done, ChecksReconciled::default());
    assert_eq!(forge.runs_read(), [id(39_282_503_375)]);
    assert_eq!(forge.writes(), []);
}

#[test]
fn a_check_whose_stamp_is_not_an_open_stamp_is_left_unjudged() {
    let stamps = [
        Some("9d92d7e9b4ef5e819bd7bf80bcf43df59d92d7e9b4ef5e819bd7bf80bcf43df5"),
        Some("pending:abc:1"),
        Some("pending:100:"),
        Some("pending:+100:1"),
        Some("pending:0:1"),
        Some("pending:100:1:2"),
        Some("pending:100:-1"),
        Some(""),
        None,
    ];
    for stamp in stamps {
        let mut unjudged = pending();
        unjudged.external_id = stamp.map(str::to_owned);
        let forge = on_b()
            .check(commit('b'), unjudged)
            .run(100, RunProgress::Completed, at(OLD));

        let done = sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default())
            .expect("the ports are up");

        assert_eq!(done.skipped, [skipped('b', Skip::NotPending)], "{stamp:?}");
        assert_eq!(done.closed, [], "{stamp:?}");
        assert_eq!(forge.runs_read(), [], "{stamp:?}");
    }
}

#[test]
fn a_check_whose_stamped_run_github_no_longer_has_is_left() {
    let forge = on_b().check(commit('b'), pending());

    let done =
        sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done.skipped, [skipped('b', Skip::RunGone)]);
    assert_eq!(forge.writes(), []);
}

#[test]
fn a_verdict_check_for_another_base_is_swept_too() {
    let forge = on_b()
        .check(commit('b'), check(1, "assay / verdict @release", STAMP))
        .run(100, RunProgress::Completed, at(OLD));

    let done =
        sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done.closed.len(), 1);
}

#[test]
fn a_deferred_check_with_a_requeue_scheduled_is_left() {
    let forge = stuck().label(id(7), "assay:llm-pending");

    let done = sweep(&forge, &deferred('b', 2), &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done, ChecksReconciled::default());
}

#[test]
fn the_label_alone_or_a_spent_deferral_does_not_keep_a_check() {
    let cases = [
        ("label, no deferral record", MemoryLedger::default(), true),
        ("label, requeues spent", deferred('b', 3), true),
        ("label, deferral of an older head", deferred('a', 0), true),
        ("deferral, no label", deferred('b', 0), false),
    ];
    for (case, ledger, labelled) in cases {
        let mut forge = stuck();
        if labelled {
            forge = forge.label(id(7), "assay:llm-pending");
        }

        let done = sweep(&forge, &ledger, &MemoryRefs::default()).expect("the ports are up");

        assert_eq!(done.closed.len(), 1, "{case}");
    }
}

#[test]
fn a_superseded_heads_check_is_cancelled() {
    let forge = on_b().check(commit('a'), pending());
    let ledger = ledger_with_pushes(&[commit('a'), commit('b')]);

    let done = sweep(&forge, &ledger, &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(
        done.closed,
        [ClosedCheck {
            pr: id(7),
            head: commit('a'),
            check: check_id(1),
            closure: Closure::Superseded,
        }]
    );
    let writes = forge.writes();
    let [write] = writes.as_slice() else {
        panic!("one write: {writes:?}");
    };
    assert_eq!(
        write.update.state,
        CheckState::Completed(CheckConclusion::Cancelled)
    );
    assert_eq!(write.update.title, "assay: cancelled (superseded)");
}

#[test]
fn a_head_another_open_pr_is_on_is_not_superseded() {
    // PR 8 is on `a…`, and the run that opened its check is live.
    let forge = on_b()
        .pull(id(8), commit('a'))
        .check(commit('a'), pending())
        .run(100, RunProgress::Live, at(OLD));
    let ledger = ledger_with_pushes(&[commit('a'), commit('b')]);

    let done = sweep(&forge, &ledger, &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done, ChecksReconciled::default());
}

#[test]
fn only_the_latest_earlier_heads_are_swept() {
    let earlier = u64::try_from(MAX_SUPERSEDED_HEADS).expect("the cap fits") + 1;
    let mut heads: Vec<CommitId> = (1..=earlier).map(head).collect();
    heads.push(commit('b'));
    let mut forge = on_b();
    for n in 1..=earlier {
        forge = forge.check(head(n), check(n, VERDICT, STAMP));
    }

    let done = sweep(&forge, &ledger_with_pushes(&heads), &MemoryRefs::default())
        .expect("the ports are up");

    // Latest first, the earlier heads are `earlier`, ..., 2, then 1: the oldest is past the cap.
    let closed: Vec<_> = done.closed.iter().map(|closed| closed.check).collect();
    let expected: Vec<_> = (2..=earlier).rev().map(check_id).collect();
    assert_eq!(closed, expected);
}

#[test]
fn the_earlier_heads_are_scanned_only_down_to_one_with_no_open_verdict_check() {
    // Latest first, PR 7's earlier heads are d, c and a; c has no open verdict check.
    let forge = on_b()
        .check(commit('d'), pending())
        .check(commit('a'), check(2, VERDICT, STAMP));
    let ledger = ledger_with_pushes(&[commit('a'), commit('c'), commit('d'), commit('b')]);

    let done = sweep(&forge, &ledger, &MemoryRefs::default()).expect("the ports are up");

    let closed: Vec<_> = done.closed.iter().map(|closed| closed.check).collect();
    assert_eq!(closed, [check_id(1)]);
    assert_eq!(forge.listed(), [commit('b'), commit('d'), commit('c')]);
}

#[test]
fn a_busy_lock_leaves_the_check() {
    let refs = MemoryRefs::default();
    refs.holding(
        &lock_ref('b'),
        &format!(
            r#"{{"schema":"assay.ledger-lock/1","run_uid":{{"repo_id":42,"run_id":9,"run_attempt":1}},"repo":"acme/widgets","head_sha":"{}","expires_at":"2026-10-12T12:01:00.000Z"}}"#,
            "b".repeat(40)
        ),
    );
    let forge = stuck();

    let done = sweep(&forge, &MemoryLedger::default(), &refs).expect("the ports are up");

    assert_eq!(done.skipped, [skipped('b', Skip::LockBusy)]);
    assert_eq!(forge.writes(), []);
}

#[test]
fn a_check_re_stamped_meanwhile_is_left() {
    let forge = stuck().restamped_to("pending:101:2");
    let refs = MemoryRefs::default();

    let done = sweep(&forge, &MemoryLedger::default(), &refs).expect("the ports are up");

    assert_eq!(done.skipped, [skipped('b', Skip::Moved)]);
    assert_eq!(forge.writes(), []);
    assert_eq!(refs.names(), Vec::<String>::new(), "the lock was released");
}

#[test]
fn a_head_that_moved_before_the_write_is_left() {
    let moved = stuck().head_moves(id(7), PullHead::Open(commit('c')));
    let done =
        sweep(&moved, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");
    assert_eq!(done.skipped, [skipped('b', Skip::HeadMoved)]);

    let superseded = on_b()
        .check(commit('a'), pending())
        .head_moves(id(7), PullHead::Open(commit('a')));
    let done = sweep(
        &superseded,
        &ledger_with_pushes(&[commit('a'), commit('b')]),
        &MemoryRefs::default(),
    )
    .expect("the ports are up");
    assert_eq!(done.skipped, [skipped('a', Skip::HeadMoved)]);
}

#[test]
fn a_superseded_check_with_no_stamp_is_left() {
    let mut unstamped = pending();
    unstamped.external_id = None;
    let forge = on_b().check(commit('a'), unstamped);
    let ledger = ledger_with_pushes(&[commit('a'), commit('b')]);

    let done = sweep(&forge, &ledger, &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done.skipped, [skipped('a', Skip::NoStamp)]);
}

#[test]
fn the_per_repo_sweep_never_reaps_a_slot_or_writes_a_cost_record() {
    let refs = MemoryRefs::default();
    refs.holding(
        "refs/heads/slot-0",
        &lease_line(1, "2026-10-12T10:00:00.000Z"),
    );
    let ledger = ledger_with_pushes(&[commit('a'), commit('b')]);
    let forge = stuck().check(commit('a'), check(2, VERDICT, "pending:99:1"));

    let done = sweep(&forge, &ledger, &refs).expect("the ports are up");

    assert_eq!(done.closed.len(), 2);
    assert_eq!(refs.names(), ["refs/heads/slot-0"]);
    assert_eq!(ledger.writes(), []);
}

#[test]
fn more_open_pulls_than_the_cap_are_refused() {
    let mut forge = FakeRepo::default();
    for number in 1..=u64::try_from(MAX_OPEN_PULLS).expect("the cap fits") + 1 {
        forge = forge.pull(id(number), commit('b'));
    }

    let refused = sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default());

    assert!(
        matches!(
            refused,
            Err(ChecksReconcileError::TooMany { what: Listing::OpenPulls, limit })
                if limit == MAX_OPEN_PULLS
        ),
        "{refused:?}"
    );
}

#[test]
fn a_head_with_more_checks_than_the_cap_refuses_the_sweep_before_any_write() {
    // PR 7's stuck check is listed first; PR 8's head then passes the cap.
    let mut forge = stuck().pull(id(8), commit('c'));
    for n in 2..=u64::try_from(MAX_HEAD_CHECKS).expect("the cap fits") + 2 {
        forge = forge.check(commit('c'), check(n, "assay / gate: temper", STAMP));
    }
    let refs = MemoryRefs::default();

    let refused = sweep(&forge, &MemoryLedger::default(), &refs);

    assert!(
        matches!(
            refused,
            Err(ChecksReconcileError::TooMany { what: Listing::Checks, limit })
                if limit == MAX_HEAD_CHECKS
        ),
        "{refused:?}"
    );
    assert_eq!(forge.writes(), []);
    assert_eq!(refs.calls(), [], "no lock was taken");
}

#[test]
fn a_failed_write_and_a_failed_release_are_both_reported() {
    let forge = stuck().writes_fail();
    let refs = MemoryRefs::default();

    let refused = reconcile_checks(
        ReconcilePorts {
            forge: &forge,
            ledger: &MemoryLedger::default(),
            refs: &NoDelete(&refs),
        },
        &request(),
    );

    assert!(
        matches!(
            refused,
            Err(ChecksReconcileError::Write {
                source: GitHubDown,
                release: Some(_)
            })
        ),
        "{refused:?}"
    );
    assert_eq!(
        refs.names(),
        [lock_ref('b')],
        "the lock expires with its TTL"
    );
}

#[test]
fn a_failed_write_still_releases_the_lock() {
    let forge = stuck().writes_fail();
    let refs = MemoryRefs::default();

    let refused = sweep(&forge, &MemoryLedger::default(), &refs);

    assert!(
        matches!(
            refused,
            Err(ChecksReconcileError::Write {
                source: GitHubDown,
                release: None
            })
        ),
        "{refused:?}"
    );
    assert_eq!(refs.names(), Vec::<String>::new());
}

#[test]
fn a_lock_lost_before_its_release_is_reported() {
    let refs = MemoryRefs::default().rival(Before::Delete, |store| {
        store.remove(&lock_ref('b'));
    });
    let forge = stuck();

    let done = sweep(&forge, &MemoryLedger::default(), &refs).expect("the ports are up");

    assert_eq!(done.closed.len(), 1);
    assert_eq!(
        done.lost_locks,
        [LostLock {
            pr: id(7),
            head: commit('b'),
            check: check_id(1),
        }]
    );
    assert!(
        refs.calls().contains(&Call::Delete {
            name: lock_ref('b'),
            deleted: false,
        }),
        "the release was a compare-and-set that found the lock gone"
    );
}

#[test]
fn github_down_stops_the_sweep() {
    let refused = sweep(
        &on_b().down(),
        &MemoryLedger::default(),
        &MemoryRefs::default(),
    );

    assert!(
        matches!(refused, Err(ChecksReconcileError::Forge(GitHubDown))),
        "{refused:?}"
    );
}
