//! `open` (S0, 04 §1, 06 §4.1): the single App verdict check per head, stamped
//! `pending:<run_id>:<seq>` under the per-head publish lock, the opener record on `opened` and
//! the push record on `synchronize` only (R3-05, R3-07, R3-09, R3-21).

use assay_app::{
    CheckConclusion, CheckState, CheckStatus, CommitId, LedgerPath, OpenError, OpenPorts,
    OpenRequest, OpenStep, OpenTrigger, PendingStamp, PushAppended, PushEvent, PushKind,
    ReopenRefused, Stamped, Unlock, UserId, open,
};
use assay_domain::RunUid;

use crate::lease_fakes::{Before, Call, MemoryRefs};
use crate::ledger_fakes::{MemoryLedger, at, commit, id, run};
use crate::open_fakes::{FakeClock, FakeGitHub, ForgeCall, GitHubDown, Reopen};

/// The triggering workflow run's `created_at`: the push clock.
const CREATED: &str = "2026-10-12T09:29:00.000Z";
const NOW: &str = "2026-10-12T09:30:00.000Z";
const VERDICT: &str = "assay / verdict";

/// The publish lock of head `bbbb…` in repository 42.
fn lock_ref() -> String {
    format!("refs/heads/lock-42-{}", "b".repeat(40))
}

/// The lock line of run `run_id` on head `bbbb…`, expiring at `expires_at`.
fn lock_line(run_id: u64, expires_at: &str) -> String {
    format!(
        r#"{{"schema":"assay.ledger-lock/1","run_uid":{{"repo_id":42,"run_id":{run_id},"run_attempt":1}},"repo":"acme/widgets","head_sha":"{}","expires_at":"{expires_at}"}}"#,
        "b".repeat(40)
    )
}

fn pushes() -> LedgerPath {
    LedgerPath::pushes(id(42), id(7))
}

/// The push line of run `run_id` numbered `seq`, from `aaaa…` to `bbbb…` by user 1001.
fn push_line(run_id: u64, seq: u64, forced: bool) -> String {
    format!(
        concat!(
            r#"{{"schema":"assay.ledger-push/1","run_uid":{{"repo_id":42,"run_id":{run_id},"#,
            r#""run_attempt":1}},"pr":7,"seq":{seq},"kind":"push","before":"{before}","#,
            r#""after":"{after}","sender_id":1001,"forced":{forced},"at":"{when}"}}"#,
            "\n"
        ),
        run_id = run_id,
        seq = seq,
        before = "a".repeat(40),
        after = "b".repeat(40),
        forced = forced,
        when = CREATED,
    )
}

/// The opener line of run `run_id` numbered `seq`, head `bbbb…`, by user 1001.
fn opener_line(run_id: u64, seq: u64) -> String {
    format!(
        concat!(
            r#"{{"schema":"assay.ledger-push/1","run_uid":{{"repo_id":42,"run_id":{run_id},"#,
            r#""run_attempt":1}},"pr":7,"seq":{seq},"kind":"opener","after":"{after}","#,
            r#""sender_id":1001,"forced":false,"at":"{when}"}}"#,
            "\n"
        ),
        run_id = run_id,
        seq = seq,
        after = "b".repeat(40),
        when = CREATED,
    )
}

/// A ledger whose PR 7 has push records numbered 1 to `count`, by runs 10, 11, ….
fn ledger_with_pushes(count: u64) -> MemoryLedger {
    let lines: String = (1..=count)
        .map(|seq| push_line(9 + seq, seq, false))
        .collect();
    MemoryLedger::default().with_file(&pushes(), lines.as_bytes())
}

fn synchronize(forced: bool) -> OpenTrigger {
    OpenTrigger::Synchronize(PushEvent {
        before: commit('a'),
        sender: UserId(id(1001)),
        forced,
    })
}

/// One run of `open`: its identity, head, trigger and base.
struct Case {
    run_uid: RunUid,
    head: CommitId,
    trigger: OpenTrigger,
    base_ref: &'static str,
}

impl Case {
    /// Run `run_id` of repository 42 on PR 7, head `bbbb…`, into `main`.
    fn new(run_id: u64, trigger: OpenTrigger) -> Self {
        Self {
            run_uid: run(run_id),
            head: commit('b'),
            trigger,
            base_ref: "main",
        }
    }

    fn request(&self) -> OpenRequest<'_> {
        OpenRequest {
            run_uid: self.run_uid,
            repo: "acme/widgets",
            pr: id(7),
            head: &self.head,
            base_ref: self.base_ref,
            default_branch: "main",
            trigger: &self.trigger,
            run_created_at: at(CREATED),
        }
    }
}

/// The ports of one run of `open` over the fakes.
fn ports<'a>(
    forge: &'a FakeGitHub<'a>,
    ledger: &'a MemoryLedger,
    refs: &'a MemoryRefs,
    clock: &'a FakeClock,
) -> OpenPorts<'a, FakeGitHub<'a>, MemoryLedger, MemoryRefs, FakeClock> {
    OpenPorts {
        forge,
        ledger,
        refs,
        clock,
    }
}

fn writes(calls: &[ForgeCall]) -> Vec<&ForgeCall> {
    calls
        .iter()
        .filter(|call| {
            matches!(
                call,
                ForgeCall::Update { .. } | ForgeCall::FindOrCreate { created: true, .. }
            )
        })
        .collect()
}

#[test]
fn a_re_run_is_refused_before_any_port_call() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = MemoryLedger::default();
    let clock = FakeClock::reading(&[NOW]);
    let mut case = Case::new(30, synchronize(false));
    case.run_uid.run_attempt = id(2);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(
        matches!(opened, Err(OpenError::NotFirstAttempt { attempt }) if attempt == id(2)),
        "{opened:?}"
    );
    assert_eq!(forge.calls(), Vec::new());
    assert_eq!(refs.calls(), Vec::new());
    assert_eq!(ledger.writes(), Vec::new());
}

#[test]
fn a_first_push_creates_the_check_in_progress_stamped_with_its_push_number() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = MemoryLedger::default();
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, synchronize(false));

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.stamped, Stamped::Created);
    assert_eq!(opened.stamp.to_string(), "pending:30:1");
    let checks = forge.checks();
    let [check] = checks.as_slice() else {
        panic!("exactly one check: {checks:?}");
    };
    assert_eq!(check.name, VERDICT);
    assert_eq!(check.head, commit('b'));
    assert_eq!(check.external_id, "pending:30:1");
    assert_eq!(check.status, CheckStatus::InProgress);
    assert_eq!(check.last.state, CheckState::InProgress);
    assert_eq!(opened.check, check.id);
}

#[test]
fn the_check_on_a_base_other_than_the_default_branch_is_named_for_that_base() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let mut case = Case::new(30, OpenTrigger::Reopened);
    case.base_ref = "release/1.2";

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.name.as_str(), "assay / verdict @release/1.2");
    let names: Vec<String> = forge.checks().into_iter().map(|check| check.name).collect();
    assert_eq!(names, ["assay / verdict @release/1.2"]);
}

#[test]
fn a_new_run_on_the_same_head_reuses_the_check_and_never_creates_a_second() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let held = forge.holding(VERDICT, "pending:20:1", CheckStatus::InProgress);
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::ReadyForReview);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.stamped, Stamped::Restamped);
    assert_eq!(opened.check, held);
    let checks = forge.checks();
    assert_eq!(checks.len(), 1, "never a second check: {checks:?}");
    assert_eq!(
        checks.first().map(|check| check.external_id.as_str()),
        Some("pending:30:1")
    );
    let calls = forge.calls();
    assert!(
        matches!(
            calls.as_slice(),
            [
                ForgeCall::FindOrCreate { created: false, .. },
                ForgeCall::Update { id, expected, .. },
            ] if *id == held && expected == "pending:20:1"
        ),
        "{calls:?}"
    );
}

#[test]
fn a_newer_run_restamps_with_the_latest_push_number() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    forge.holding(VERDICT, "pending:11:2", CheckStatus::InProgress);
    let ledger = ledger_with_pushes(3);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.stamp.to_string(), "pending:30:3");
    let update = forge.checks().into_iter().map(|check| check.last).next();
    let update = update.expect("one check");
    assert_eq!(update.external_id, "pending:30:3");
    assert_eq!(update.state, CheckState::InProgress);
}

#[test]
fn an_older_run_yields_to_a_newer_runs_stamp() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    forge.holding(VERDICT, "pending:40:3", CheckStatus::InProgress);
    let ledger = ledger_with_pushes(3);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(
        opened.stamped,
        Stamped::Yielded {
            found: "pending:40:3".to_owned()
        }
    );
    assert_eq!(writes(&forge.calls()), Vec::<&ForgeCall>::new());
    assert!(refs.names().is_empty(), "the lock is released");
}

#[test]
fn a_check_this_run_already_stamped_in_progress_is_not_written_again() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    forge.holding(VERDICT, "pending:30:1", CheckStatus::InProgress);
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.stamped, Stamped::Unchanged);
    assert_eq!(writes(&forge.calls()), Vec::<&ForgeCall>::new());
}

#[test]
fn every_check_write_is_made_holding_the_lock_which_is_released_after() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref()).reopening(Reopen::KeepsCompleted);
    forge.holding(VERDICT, &"0f".repeat(32), CheckStatus::Completed);
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, synchronize(false));

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    let calls = forge.calls();
    assert_eq!(
        writes(&calls).len(),
        2,
        "re-stamp and re-deciding: {calls:?}"
    );
    for call in &calls {
        let locked = match call {
            ForgeCall::FindOrCreate { locked, .. } | ForgeCall::Update { locked, .. } => *locked,
            ForgeCall::Unexpected(name) => panic!("open reads no PR content: {name}"),
        };
        assert!(locked, "made without the lock: {call:?}");
    }
    assert!(
        matches!(opened.unlock, Unlock::Released),
        "{:?}",
        opened.unlock
    );
    assert_eq!(refs.names(), Vec::<String>::new());
    let lock_calls: Vec<Call> = refs
        .calls()
        .into_iter()
        .filter(|call| !matches!(call, Call::Read { .. }))
        .collect();
    assert_eq!(
        lock_calls,
        [
            Call::Create {
                name: lock_ref(),
                created: true
            },
            Call::Delete {
                name: lock_ref(),
                deleted: true
            },
        ]
    );
}

#[test]
fn a_busy_lock_is_retried_three_times_then_the_run_aborts_without_a_write() {
    let refs = MemoryRefs::default();
    refs.holding(&lock_ref(), &lock_line(99, "2026-10-12T09:31:00.000Z"));
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(
        matches!(opened, Err(OpenError::LockBusy { attempts: 4 })),
        "{opened:?}"
    );
    assert_eq!(clock.pauses(), [1, 2, 3]);
    assert_eq!(forge.calls(), Vec::new());
    assert_eq!(
        refs.get(&lock_ref()).map(|(_, message)| message),
        Some(lock_line(99, "2026-10-12T09:31:00.000Z")),
        "the other writer's lock is untouched"
    );
}

#[test]
fn a_lock_freed_during_a_pause_is_taken_on_the_retry() {
    let refs = MemoryRefs::default()
        .rival(Before::Create, |_| {})
        .rival(Before::Create, |store| store.remove(&lock_ref()));
    refs.holding(&lock_ref(), &lock_line(99, "2026-10-12T09:31:00.000Z"));
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.stamped, Stamped::Created);
    assert_eq!(clock.pauses(), [1]);
}

#[test]
fn a_lock_whose_lease_ran_out_before_the_write_aborts_without_a_write() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW, "2026-10-12T09:32:00.000Z"]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(
        matches!(
            opened,
            Err(OpenError::LockExpired {
                unlock: Unlock::Released | Unlock::Failed(_)
            })
        ),
        "{opened:?}"
    );
    assert_eq!(forge.calls(), Vec::new());
}

#[test]
fn a_clock_that_cannot_be_read_aborts_before_the_lock() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::broken();
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(
        matches!(
            opened,
            Err(OpenError::Clock {
                unlock: Unlock::NotTaken,
                ..
            })
        ),
        "{opened:?}"
    );
    assert_eq!(refs.calls(), Vec::new());
    assert_eq!(forge.calls(), Vec::new());
}

#[test]
fn a_synchronize_appends_its_push_with_the_runs_created_at() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, synchronize(true));

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(
        ledger.text(&pushes()),
        format!("{}{}", push_line(10, 1, false), push_line(30, 2, true))
    );
    let Some(PushAppended::Recorded(record)) = &opened.push else {
        panic!("the push was recorded: {:?}", opened.push);
    };
    assert_eq!(record.seq, id(2));
    assert_eq!(record.push.at, at(CREATED));
    assert_eq!(opened.stamp.to_string(), "pending:30:2");
}

#[test]
fn events_other_than_opened_and_a_push_never_touch_the_push_records() {
    for trigger in [
        OpenTrigger::Reopened,
        OpenTrigger::ReadyForReview,
        OpenTrigger::Edited,
        OpenTrigger::Dispatch,
    ] {
        let refs = MemoryRefs::default();
        let forge = FakeGitHub::new(&refs, &lock_ref());
        let ledger = ledger_with_pushes(2);
        let clock = FakeClock::reading(&[NOW]);
        let case = Case::new(30, trigger);

        let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

        assert_eq!(opened.push, None, "{:?}", case.trigger);
        assert_eq!(ledger.writes(), Vec::new(), "{:?}", case.trigger);
        assert_eq!(
            ledger.text(&pushes()),
            format!("{}{}", push_line(10, 1, false), push_line(11, 2, false))
        );
        assert_eq!(opened.stamp.to_string(), "pending:30:2");
    }
}

#[test]
fn an_opened_pr_records_its_sender_as_the_opener_with_the_runs_created_at() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = MemoryLedger::default();
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(
        30,
        OpenTrigger::Opened {
            sender: UserId(id(1001)),
        },
    );

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(ledger.text(&pushes()), opener_line(30, 1));
    let Some(PushAppended::Recorded(record)) = &opened.push else {
        panic!("the opener was recorded: {:?}", opened.push);
    };
    assert_eq!(record.push.kind, PushKind::Opener);
    assert_eq!(record.push.sender_id, UserId(id(1001)));
    assert_eq!(record.push.after, commit('b'));
    assert_eq!(record.push.at, at(CREATED));
    assert_eq!(opened.stamp, PendingStamp::new(id(30), 1));
}

#[test]
fn a_reopened_pr_with_no_push_record_is_stamped_number_zero() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = MemoryLedger::default();
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Reopened);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.stamp, PendingStamp::new(id(30), 0));
    assert_eq!(ledger.writes(), Vec::new());
}

#[test]
fn a_push_record_that_cannot_be_appended_aborts_before_the_lock_and_the_check() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = MemoryLedger::default().down();
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, synchronize(false));

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(matches!(opened, Err(OpenError::Push(_))), "{opened:?}");
    assert_eq!(refs.calls(), Vec::new());
    assert_eq!(forge.calls(), Vec::new());
}

#[test]
fn push_records_that_cannot_be_read_abort_before_the_lock_and_the_check() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    let ledger = MemoryLedger::default().down();
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Reopened);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(matches!(opened, Err(OpenError::Pushes(_))), "{opened:?}");
    assert_eq!(refs.calls(), Vec::new());
    assert_eq!(forge.calls(), Vec::new());
}

#[test]
fn a_completed_check_is_reopened_in_progress_with_its_conclusion_cleared() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref());
    forge.holding(VERDICT, &"0f".repeat(32), CheckStatus::Completed);
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Reopened);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(opened.stamped, Stamped::Reopened);
    let check = forge.checks().into_iter().next().expect("one check");
    assert_eq!(check.status, CheckStatus::InProgress);
    assert_eq!(check.external_id, "pending:30:1");
    assert_eq!(check.last.state, CheckState::InProgress);
}

/// Each check `forge` holds: its status, stamp, and the state and title last written.
fn shown(forge: &FakeGitHub<'_>) -> Vec<(CheckStatus, String, CheckState, String)> {
    forge
        .checks()
        .into_iter()
        .map(|check| {
            (
                check.status,
                check.external_id,
                check.last.state,
                check.last.title,
            )
        })
        .collect()
}

/// What [`shown`] gives after `open` published `failure (re-deciding)` for run 30.
fn re_deciding() -> Vec<(CheckStatus, String, CheckState, String)> {
    vec![(
        CheckStatus::Completed,
        "pending:30:1".to_owned(),
        CheckState::Completed(CheckConclusion::Failure),
        "assay: failure (re-deciding)".to_owned(),
    )]
}

#[test]
fn a_completed_check_github_keeps_completed_is_published_failing_re_deciding() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref()).reopening(Reopen::KeepsCompleted);
    forge.holding(VERDICT, &"0f".repeat(32), CheckStatus::Completed);
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Reopened);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(
        opened.stamped,
        Stamped::ReDeciding(ReopenRefused::KeptConclusion)
    );
    assert_eq!(shown(&forge), re_deciding());
}

#[test]
fn a_completed_check_whose_reopening_github_refuses_is_published_failing_re_deciding() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref()).reopening(Reopen::Rejects);
    forge.holding(VERDICT, &"0f".repeat(32), CheckStatus::Completed);
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Reopened);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock)).expect("opened");

    assert_eq!(
        opened.stamped,
        Stamped::ReDeciding(ReopenRefused::Rejected(GitHubDown))
    );
    assert_eq!(shown(&forge), re_deciding());
}

#[test]
fn a_forge_failure_aborts_visibly_and_releases_the_lock() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref()).down();
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(
        matches!(
            opened,
            Err(OpenError::Forge {
                step: OpenStep::FindOrCreate,
                unlock: Unlock::Released,
                ..
            })
        ),
        "{opened:?}"
    );
    assert_eq!(refs.names(), Vec::<String>::new());
    assert_eq!(
        forge.calls().len(),
        1,
        "the adapter already retried ×3; open does not call again"
    );
}

#[test]
fn a_stamp_another_writer_moved_under_the_lock_is_refused() {
    let refs = MemoryRefs::default();
    let forge = FakeGitHub::new(&refs, &lock_ref()).rival_stamps("pending:50:1");
    forge.holding(VERDICT, "pending:20:1", CheckStatus::InProgress);
    let ledger = ledger_with_pushes(1);
    let clock = FakeClock::reading(&[NOW]);
    let case = Case::new(30, OpenTrigger::Dispatch);

    let opened = open(&case.request(), &ports(&forge, &ledger, &refs, &clock));

    assert!(
        matches!(
            &opened,
            Err(OpenError::Moved {
                found,
                unlock: Unlock::Released,
            }) if found == "pending:50:1"
        ),
        "{opened:?}"
    );
}

#[test]
fn a_pending_stamp_reads_back_only_in_the_form_open_writes() {
    let stamp = PendingStamp::new(id(30), 2);

    assert_eq!(stamp.to_string(), "pending:30:2");
    assert_eq!(PendingStamp::parse("pending:30:2"), Some(stamp));
    for text in [
        "pending:30",
        "pending:0:2",
        "pending:30:",
        "pending::2",
        "pending:30:2:1",
        "pending:+30:2",
        "pending:30:-2",
        "Pending:30:2",
        "pending:030:2",
        &"0f".repeat(32),
    ] {
        assert_eq!(PendingStamp::parse(text), None, "{text}");
    }
}
