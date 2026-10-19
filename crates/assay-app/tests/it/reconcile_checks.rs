//! The per-repo reconcile (06 §4, 02 §5, R3-06, R3-16): a consumer repository closes its own
//! stuck verdict checks as `failure (publish-error)` and its superseded heads' as `cancelled`,
//! each under the head's publish lock and by compare-and-set on the check's `external_id`.

use assay_app::{
    CheckConclusion, CheckRunId, CheckState, CheckStatus, ChecksReconcileError,
    ChecksReconcileRequest, ChecksReconciled, ClosedCheck, Closure, CommitId, LedgerPath, Listing,
    MAX_OPEN_PULLS, MAX_SUPERSEDED_HEADS, PullHead, ReconcilePorts, Skip, SkippedCheck,
    reconcile_checks,
};
use assay_domain::RunUid;

use crate::lease_fakes::{Call, MemoryRefs, RefsDown, lease_line};
use crate::ledger_fakes::{Down, MemoryLedger, at, commit, id};
use crate::reconcile_fakes::{FakeRepo, GitHubDown, check, head};

const NOW: &str = "2026-10-12T12:00:00.000Z";
/// Started 50 minutes and a millisecond before `NOW`: stuck.
const OLD: &str = "2026-10-12T11:09:59.999Z";
/// Started exactly 50 minutes before `NOW`: not yet stuck.
const DUE: &str = "2026-10-12T11:10:00.000Z";
const VERDICT: &str = "assay / verdict";
const PIPELINE: &str = ".github/workflows/assay.yml";

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

fn pending(started: &str) -> assay_app::ReconcileCheck {
    check(1, VERDICT, "pending:100:1", at(started))
}

fn check_id(n: u64) -> CheckRunId {
    CheckRunId(id(n))
}

/// A repository with PR 7 open on head `b…`.
fn on_b() -> FakeRepo {
    FakeRepo::default().pull(id(7), commit('b'))
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
                    r#""run_attempt":1}},"pr":7,"seq":{seq},"before":"{before}","after":"{after}","#,
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
fn a_stuck_verdict_check_is_closed_as_a_publish_error_under_the_lock() {
    let forge = on_b().check(commit('b'), pending(OLD));
    let ledger = MemoryLedger::default();
    let refs = MemoryRefs::default();

    let done = sweep(&forge, &ledger, &refs).expect("the ports are up");

    assert_eq!(
        done.closed,
        [ClosedCheck {
            pr: id(7),
            head: commit('b'),
            check: check_id(1),
            closure: Closure::Stuck,
        }]
    );
    let writes = forge.writes();
    let [write] = writes.as_slice() else {
        panic!("one write: {writes:?}");
    };
    assert_eq!(write.expected, "pending:100:1");
    assert_eq!(
        write.update.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    assert_eq!(write.update.title, "assay: failure (publish-error)");
    assert_eq!(write.update.text, "");
    assert_eq!(write.update.external_id, "reconcile:5000");
    let creates = refs
        .calls()
        .into_iter()
        .filter(
            |call| matches!(call, Call::Create { name, created: true } if *name == lock_ref('b')),
        )
        .count();
    assert_eq!(creates, 1, "the head's publish lock was taken");
    assert_eq!(refs.names(), Vec::<String>::new(), "and released");
}

#[test]
fn checks_that_are_not_stuck_are_left() {
    let cases: [(&str, FakeRepo); 5] = [
        ("young", on_b().check(commit('b'), pending(DUE))),
        (
            "a live pipeline run",
            on_b()
                .check(commit('b'), pending(OLD))
                .live_run(commit('b'), PIPELINE),
        ),
        (
            "a live decide-only run",
            on_b()
                .check(commit('b'), pending(OLD))
                .live_run(commit('b'), ".github/workflows/assay-decide.yml"),
        ),
        (
            "a gate check",
            on_b().check(commit('b'), check(1, "assay / gate: temper", "x", at(OLD))),
        ),
        ("a completed check", {
            let mut done = pending(OLD);
            done.status = CheckStatus::Completed;
            on_b().check(commit('b'), done)
        }),
    ];
    for (case, forge) in cases {
        let done = sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default())
            .expect("the ports are up");

        assert_eq!(done, ChecksReconciled::default(), "{case}");
        assert_eq!(forge.writes(), [], "{case}");
    }
}

#[test]
fn another_workflow_running_on_the_head_does_not_keep_a_check_alive() {
    let forge = on_b()
        .check(commit('b'), pending(OLD))
        .live_run(commit('b'), ".github/workflows/rust.yml");

    let done =
        sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done.closed.len(), 1);
}

#[test]
fn a_verdict_check_for_another_base_is_swept_too() {
    let forge = on_b().check(
        commit('b'),
        check(1, "assay / verdict @release", "pending:1:1", at(OLD)),
    );

    let done =
        sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done.closed.len(), 1);
}

#[test]
fn a_deferred_check_with_a_requeue_scheduled_is_left() {
    let forge = on_b()
        .check(commit('b'), pending(OLD))
        .label(id(7), "assay:llm-pending");

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
        let mut forge = on_b().check(commit('b'), pending(OLD));
        if labelled {
            forge = forge.label(id(7), "assay:llm-pending");
        }

        let done = sweep(&forge, &ledger, &MemoryRefs::default()).expect("the ports are up");

        assert_eq!(done.closed.len(), 1, "{case}");
    }
}

#[test]
fn a_superseded_heads_check_is_cancelled() {
    let forge = on_b().check(commit('a'), pending(DUE));
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
    let forge = on_b()
        .pull(id(8), commit('a'))
        .check(commit('a'), pending(DUE));
    let ledger = ledger_with_pushes(&[commit('a'), commit('b')]);

    let done = sweep(&forge, &ledger, &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done, ChecksReconciled::default());
}

#[test]
fn only_the_latest_earlier_heads_are_swept() {
    let mut heads: Vec<CommitId> = (1..=17).map(head).collect();
    heads.push(commit('b'));
    // Latest first, the earlier heads are 17, 16, ..., 2 (sixteen of them), then 1.
    let forge = on_b()
        .check(head(2), pending(DUE))
        .check(head(1), check(2, VERDICT, "pending:1:1", at(DUE)));

    let done = sweep(&forge, &ledger_with_pushes(&heads), &MemoryRefs::default())
        .expect("the ports are up");

    assert_eq!(MAX_SUPERSEDED_HEADS, 16);
    let closed: Vec<_> = done.closed.iter().map(|closed| closed.check).collect();
    assert_eq!(closed, [check_id(1)]);
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
    let forge = on_b().check(commit('b'), pending(OLD));

    let done = sweep(&forge, &MemoryLedger::default(), &refs).expect("the ports are up");

    assert_eq!(done.skipped, [skipped('b', Skip::LockBusy)]);
    assert_eq!(forge.writes(), []);
}

#[test]
fn a_check_re_stamped_meanwhile_is_left() {
    let forge = on_b()
        .check(commit('b'), pending(OLD))
        .restamped_to("pending:101:2");
    let refs = MemoryRefs::default();

    let done = sweep(&forge, &MemoryLedger::default(), &refs).expect("the ports are up");

    assert_eq!(done.skipped, [skipped('b', Skip::Moved)]);
    assert_eq!(forge.writes(), []);
    assert_eq!(refs.names(), Vec::<String>::new(), "the lock was released");
}

#[test]
fn a_head_that_moved_before_the_write_is_left() {
    let stuck = on_b()
        .check(commit('b'), pending(OLD))
        .head_moves(id(7), PullHead::Open(commit('c')));
    let done =
        sweep(&stuck, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");
    assert_eq!(done.skipped, [skipped('b', Skip::HeadMoved)]);

    let superseded = on_b()
        .check(commit('a'), pending(DUE))
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
fn a_check_with_no_stamp_is_left() {
    let mut unstamped = pending(OLD);
    unstamped.external_id = None;
    let forge = on_b().check(commit('b'), unstamped);

    let done =
        sweep(&forge, &MemoryLedger::default(), &MemoryRefs::default()).expect("the ports are up");

    assert_eq!(done.skipped, [skipped('b', Skip::NoStamp)]);
}

#[test]
fn the_per_repo_sweep_never_reaps_a_slot_or_writes_a_cost_record() {
    let refs = MemoryRefs::default();
    refs.holding(
        "refs/heads/slot-0",
        &lease_line(1, "2026-10-12T10:00:00.000Z"),
    );
    let ledger = ledger_with_pushes(&[commit('a'), commit('b')]);
    let forge = on_b()
        .check(commit('b'), pending(OLD))
        .check(commit('a'), check(2, VERDICT, "pending:99:1", at(DUE)));

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

fn skipped(head: char, reason: Skip) -> SkippedCheck {
    SkippedCheck {
        pr: id(7),
        head: commit(head),
        check: check_id(1),
        reason,
    }
}

#[test]
fn the_stuck_threshold_is_twice_the_full_tiers_p95() {
    assert_eq!(assay_app::STUCK_AFTER.as_secs(), 50 * 60);
}
