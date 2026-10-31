//! `run_open` (#60 ST-4, S0) over fakes: which event writes which ledger record, that the
//! record's `at` is the run's `created_at` (never the clock's), that a `synchronize`'s
//! `forced` is the compare's answer, and that a failed read writes nothing.

use std::ffi::OsString;
use std::num::NonZeroU64;

use assay_app::{
    CheckState, CommitId, LedgerTime, OpenPorts, Opened, PushAppended, PushDraft, PushKind,
    PushRecord, Stamped, UserId,
};
use assay_cli::outputs::{Identity, Trigger};
use assay_cli::{FirstAttempt, OpenJob, OpenRunError, RunEnv, RunVariables, run_open};
use assay_domain::RunUid;

use crate::open_fakes::{
    FakeGitHub, FakeReads, FixedClock, MemoryLedger, MemoryRefs, Read, Refused,
};

const RUN_ID: u64 = 39_168_909_542;
const REPO_ID: u64 = 42;
const CREATED_AT: &str = "2026-10-31T09:00:00.000Z";
const NOW: &str = "2026-10-31T09:01:30.000Z";

fn id(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("a positive test id")
}

fn at(text: &str) -> LedgerTime {
    text.parse().expect("a ledger time")
}

fn commit(digit: char) -> CommitId {
    digit
        .to_string()
        .repeat(40)
        .parse()
        .expect("a full commit id")
}

fn first_attempt() -> FirstAttempt {
    let vars = RunVariables {
        run_id: Some(OsString::from(RUN_ID.to_string())),
        run_attempt: Some(OsString::from("1")),
        repository_id: Some(OsString::from(REPO_ID.to_string())),
        repository: Some(OsString::from("Wet-Ink-Corporation/assay")),
    };
    RunEnv::from_vars(&vars)
        .expect("valid run variables")
        .first_attempt()
        .expect("a first attempt")
}

/// PR 7 into `main`, head `bbbb…`, authored by 11 and sent by 12.
fn identity(trigger: Trigger) -> Identity {
    Identity {
        pr: id(7),
        head_sha: commit('b'),
        base_sha: commit('a'),
        base_ref: "main".to_owned(),
        draft: false,
        author: UserId(id(11)),
        sender: UserId(id(12)),
        trigger,
        default_branch: "main".to_owned(),
    }
}

/// The fakes `run_open` writes through.
#[derive(Default)]
struct World {
    github: FakeGitHub,
    ledger: MemoryLedger,
    refs: MemoryRefs,
}

type Outcome = Result<
    Opened<Refused, Refused>,
    OpenRunError<Refused, assay_app::OpenError<Refused, Refused, Refused, Refused>>,
>;

impl World {
    fn open(&self, trigger: Trigger, reads: &FakeReads) -> Outcome {
        let run = first_attempt();
        let identity = identity(trigger);
        let clock = FixedClock::at(at(NOW));
        let ports = OpenPorts {
            forge: &self.github,
            ledger: &self.ledger,
            refs: &self.refs,
            clock: &clock,
        };
        run_open(
            OpenJob {
                run: &run,
                identity: &identity,
            },
            reads,
            &ports,
        )
    }
}

fn run_uid() -> RunUid {
    RunUid {
        repo_id: id(REPO_ID),
        run_id: id(RUN_ID),
        run_attempt: id(1),
    }
}

#[test]
fn an_opened_pr_gets_its_opener_record_and_an_in_progress_stamp() {
    let world = World::default();
    let reads = FakeReads::new(at(CREATED_AT), false);

    let opened = world
        .open(Trigger::Opened, &reads)
        .expect("the check opens");

    assert_eq!(
        opened.push,
        Some(PushAppended::Recorded(PushRecord {
            seq: id(1),
            push: PushDraft {
                run_uid: run_uid(),
                pr: id(7),
                kind: PushKind::Opener,
                after: commit('b'),
                sender_id: UserId(id(12)),
                at: at(CREATED_AT),
            },
        }))
    );
    assert_eq!(
        (
            opened.stamped,
            opened.stamp.to_string(),
            opened.name.as_str()
        ),
        (
            Stamped::Created,
            format!("pending:{RUN_ID}:1"),
            "assay / verdict"
        )
    );
    let checks = world.github.checks();
    assert_eq!(
        checks
            .iter()
            .map(|check| (
                check.head.clone(),
                check.run.external_id.as_str(),
                check.state
            ))
            .collect::<Vec<_>>(),
        [(
            commit('b'),
            format!("pending:{RUN_ID}:1").as_str(),
            CheckState::InProgress
        )]
    );
    assert_eq!(reads.reads(), [Read::CreatedAt(id(RUN_ID))]);
}

#[test]
fn the_records_time_is_the_runs_created_at_and_the_clock_times_only_the_lease() {
    let world = World::default();
    let reads = FakeReads::new(at(CREATED_AT), false);

    world
        .open(Trigger::Opened, &reads)
        .expect("the check opens");

    let files = world.ledger.files();
    let record = files
        .get(&format!("pushes/{REPO_ID}/7.jsonl"))
        .expect("the opener record");
    assert!(record.contains(CREATED_AT), "{record}");
    assert!(!record.contains(NOW), "{record}");
    assert_eq!(
        (world.refs.names(), world.refs.deleted().len()),
        (Vec::<String>::new(), 1),
        "the lock was taken and released"
    );
}

#[test]
fn a_synchronize_records_a_push_forced_as_the_compare_says() {
    for forced in [true, false] {
        let world = World::default();
        let reads = FakeReads::new(at(CREATED_AT), forced);

        let opened = world
            .open(
                Trigger::Synchronize {
                    before: commit('c'),
                },
                &reads,
            )
            .expect("the check opens");

        assert_eq!(
            opened
                .push
                .as_ref()
                .map(|appended| appended.record().push.kind.clone()),
            Some(PushKind::Push {
                before: commit('c'),
                forced,
            }),
            "forced {forced}"
        );
        assert_eq!(
            reads.reads(),
            [
                Read::CreatedAt(id(RUN_ID)),
                Read::Forced {
                    before: commit('c'),
                    after: commit('b'),
                },
            ]
        );
    }
}

#[test]
fn other_events_write_no_ledger_record_and_stamp_the_latest_number() {
    for trigger in [
        Trigger::Reopened,
        Trigger::ReadyForReview,
        Trigger::Edited,
        Trigger::Dispatch,
    ] {
        let world = World::default();
        let reads = FakeReads::new(at(CREATED_AT), true);

        let opened = world
            .open(trigger.clone(), &reads)
            .expect("the check opens");

        assert_eq!(
            (
                opened.push,
                world.ledger.files().len(),
                opened.stamp.to_string()
            ),
            (None, 0, format!("pending:{RUN_ID}:0")),
            "{trigger:?}"
        );
        assert_eq!(reads.reads(), [Read::CreatedAt(id(RUN_ID))], "{trigger:?}");
    }
}

#[test]
fn an_unreadable_run_stops_open_before_any_write() {
    let world = World::default();
    let reads = FakeReads::new(at(CREATED_AT), false).run_unreadable();

    let opened = world.open(Trigger::Opened, &reads);

    assert!(
        matches!(
            opened,
            Err(OpenRunError::RunCreatedAt(Refused("run_created_at")))
        ),
        "{opened:?}"
    );
    assert_eq!(
        (
            world.ledger.files().len(),
            world.github.checks().len(),
            world.refs.deleted().len()
        ),
        (0, 0, 0)
    );
}

#[test]
fn an_unreadable_compare_stops_a_synchronize_before_any_write() {
    let world = World::default();
    let reads = FakeReads::new(at(CREATED_AT), false).compare_unreadable();

    let opened = world.open(
        Trigger::Synchronize {
            before: commit('c'),
        },
        &reads,
    );

    assert!(
        matches!(opened, Err(OpenRunError::Forced(Refused("forced")))),
        "{opened:?}"
    );
    assert_eq!(
        (
            world.ledger.files().len(),
            world.github.checks().len(),
            world.refs.deleted().len()
        ),
        (0, 0, 0)
    );
}
