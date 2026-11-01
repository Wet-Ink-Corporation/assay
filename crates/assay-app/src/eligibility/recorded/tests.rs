//! The strict reader of `github` against its writer: every snapshot [`StateRecord::of`] writes
//! reads back as the facts it was written from, and anything the writer never writes is
//! refused (wi-b5affa).

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use assay_domain::{Fingerprint, OverrideScope, RequiredRole};
use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use serde_json::Value;

use super::{RecordedState, StateRecordIn};
use crate::config::{CommitId, EligibilityLists, Sha256Digest};
use crate::eligibility::approvals::{Approval, ApprovalStatus};
use crate::eligibility::commands::{CommandStatus, JudgedCommand, SnapshotCommand, VoidCause};
use crate::eligibility::grammar::{AckedSet, Command};
use crate::eligibility::record::StateRecord;
use crate::eligibility::rule::{Ineligible, RangeFacts, Rule};
use crate::eligibility::state::{CommitRange, GitHubState};
use crate::eligibility::timestamp::Timestamp;
use crate::publish::CommentId;
use crate::status::UserId;

/// Every reason an id is ineligible. The match has no wildcard, so a new reason does not
/// compile until it is listed here.
const INELIGIBLE: [Ineligible; 8] = [
    Ineligible::NotPrivileged,
    Ineligible::Maintainer,
    Ineligible::Agent,
    Ineligible::Author,
    Ineligible::InRange,
    Ineligible::Pusher,
    Ineligible::UnattributedRange,
    Ineligible::RangeTooLarge,
];

/// Every cause a record no longer stands, listed as [`INELIGIBLE`] is.
const VOID: [VoidCause; 5] = [
    VoidCause::Deleted,
    VoidCause::Edited,
    VoidCause::Hash,
    VoidCause::Author,
    VoidCause::App,
];

#[test]
fn every_ineligible_reason_and_void_cause_is_listed() {
    for why in INELIGIBLE {
        match why {
            Ineligible::NotPrivileged
            | Ineligible::Maintainer
            | Ineligible::Agent
            | Ineligible::Author
            | Ineligible::InRange
            | Ineligible::Pusher
            | Ineligible::UnattributedRange
            | Ineligible::RangeTooLarge => {}
        }
    }
    for cause in VOID {
        match cause {
            VoidCause::Deleted
            | VoidCause::Edited
            | VoidCause::Hash
            | VoidCause::Author
            | VoidCause::App => {}
        }
    }
}

#[test]
fn every_code_the_writer_writes_reads_back_as_its_reason() {
    for why in INELIGIBLE {
        assert_eq!(super::ineligible(why.code()), Some(why), "{why:?}");
    }
    for cause in VOID {
        assert_eq!(super::void_cause(cause.code()), Some(cause), "{cause:?}");
    }
}

fn id(raw: u64) -> UserId {
    UserId(NonZeroU64::new(raw).expect("generated ids start at one"))
}

fn commit(digit: char) -> CommitId {
    digit
        .to_string()
        .repeat(40)
        .parse()
        .expect("forty hex digits")
}

fn fp() -> Fingerprint {
    "0123456789abcdef".parse().expect("a fingerprint")
}

fn digest(digit: char) -> Sha256Digest {
    format!("sha256:{}", digit.to_string().repeat(64))
        .parse()
        .expect("a digest")
}

/// One command of each recorded kind.
fn commands() -> [Command; 6] {
    [
        Command::Waive { finding: fp() },
        Command::Override {
            scope: OverrideScope::All,
            reasons_digest: digest('a'),
            reason: "the gate is down".to_owned(),
        },
        Command::Override {
            scope: OverrideScope::Llm,
            reasons_digest: digest('b'),
            reason: "x".to_owned(),
        },
        Command::Dismiss {
            finding: fp(),
            reason: "not a bug here".to_owned(),
        },
        Command::Retry {
            role: RequiredRole::Combined,
        },
        Command::WeighInAck {
            set: AckedSet::None,
        },
    ]
}

/// `.assay/owners` and `.assay/agents` listing these ids, each with login `u<id>`.
fn lists(
    owners: &[u64],
    approvers: &[u64],
    maintainers: &[u64],
    agents: &[u64],
) -> EligibilityLists {
    let entries = |ids: &[u64]| {
        ids.iter()
            .map(|id| format!("{{ id = {id}, login = \"u{id}\" }}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let owners_file = format!(
        "owners = [{}]\napprovers = [{}]\nmaintainers = [{}]",
        entries(owners),
        entries(approvers),
        entries(maintainers)
    );
    let agents_file = format!("agents = [{}]", entries(agents));
    EligibilityLists::parse(Some(&owners_file), Some(&agents_file)).expect("disjoint lists")
}

fn timestamp() -> impl Strategy<Value = Timestamp> {
    (
        0..24_u32,
        0..60_u32,
        0..60_u32,
        prop_oneof![Just(0_u32), 0..1_000_000_000_u32],
    )
        .prop_map(|(hour, minute, second, nanos)| {
            format!("2026-10-13T{hour:02}:{minute:02}:{second:02}.{nanos:09}Z")
                .parse()
                .expect("an RFC 3339 time")
        })
}

fn ineligible() -> impl Strategy<Value = Ineligible> {
    proptest::sample::select(INELIGIBLE.to_vec())
}

fn approval() -> impl Strategy<Value = Approval> {
    let status = prop_oneof![
        Just(ApprovalStatus::Counted),
        timestamp().prop_map(ApprovalStatus::BeforePush),
        Just(ApprovalStatus::NoPushClock),
        Just(ApprovalStatus::Unsubmitted),
        ineligible().prop_map(ApprovalStatus::Ineligible),
    ];
    (
        1..1000_u64,
        1..200_u64,
        proptest::option::of(timestamp()),
        status,
    )
        .prop_map(|(review, reviewer, submitted_at, status)| Approval {
            review: NonZeroU64::new(review).expect("from one"),
            reviewer: id(reviewer),
            submitted_at,
            status,
        })
}

/// `approval` as `approvals` could judge it under push clock `clock`: an approval is before
/// the push only when it has a time and the head a clock, which it names.
fn consistent(approval: Approval, clock: Option<Timestamp>) -> Approval {
    let status = match (approval.status, approval.submitted_at, clock) {
        (ApprovalStatus::BeforePush(_), None, _) => ApprovalStatus::Unsubmitted,
        (ApprovalStatus::BeforePush(_), Some(_), None) => ApprovalStatus::NoPushClock,
        (ApprovalStatus::BeforePush(_), Some(_), Some(clock)) => ApprovalStatus::BeforePush(clock),
        (status, _, _) => status,
    };
    Approval { status, ..approval }
}

fn command_status() -> impl Strategy<Value = CommandStatus> {
    prop_oneof![
        Just(CommandStatus::Valid),
        proptest::sample::select(VOID.to_vec()).prop_map(CommandStatus::Void),
        Just(CommandStatus::OtherHead),
        ineligible().prop_map(CommandStatus::Ineligible),
        Just(CommandStatus::BeforePushClock),
    ]
}

fn judged() -> impl Strategy<Value = JudgedCommand> {
    (
        1..1000_u64,
        1..200_u64,
        timestamp(),
        proptest::sample::select(vec!['a', 'b', 'c']),
        proptest::sample::select(commands().to_vec()),
        command_status(),
    )
        .prop_map(
            |(comment, actor, created_at, head, command, status)| JudgedCommand {
                record: SnapshotCommand {
                    comment_id: CommentId(NonZeroU64::new(comment).expect("from one")),
                    actor: id(actor),
                    created_at,
                    head_sha: commit(head),
                    body_sha256: digest('c'),
                    command,
                },
                status,
            },
        )
}

fn range() -> impl Strategy<Value = RangeFacts> {
    prop_oneof![
        (btree_set(1..200_u64, 0..5), any::<bool>()).prop_map(|(users, unattributed)| {
            RangeFacts::Attributed {
                users: users.into_iter().map(id).collect(),
                unattributed,
            }
        }),
        Just(RangeFacts::TooLarge),
    ]
}

/// A snapshot as `github_state` could take it, with disjoint lists by construction.
fn snapshot() -> impl Strategy<Value = GitHubState> {
    let ids = (
        btree_set(1..40_u64, 0..4),
        btree_set(41..80_u64, 0..4),
        btree_set(81..120_u64, 0..3),
        btree_set(121..160_u64, 0..3),
    );
    (
        ids,
        1..200_u64,
        range(),
        btree_set(1..200_u64, 0..5),
        proptest::option::of(timestamp()),
        vec(approval(), 0..4),
        vec(judged(), 0..4),
    )
        .prop_map(
            |(
                (owners, approvers, maintainers, agents),
                author,
                range,
                pushers,
                clock,
                approvals,
                commands,
            )| {
                let as_vec = |set: BTreeSet<u64>| set.into_iter().collect::<Vec<_>>();
                let approvals = approvals
                    .into_iter()
                    .map(|approval| consistent(approval, clock))
                    .collect();
                let commits = match range {
                    RangeFacts::Attributed { .. } => CommitRange::Within,
                    RangeFacts::TooLarge => CommitRange::NeedsSplit,
                };
                GitHubState {
                    head: commit('b'),
                    rule: Rule {
                        lists: lists(
                            &as_vec(owners),
                            &as_vec(approvers),
                            &as_vec(maintainers),
                            &as_vec(agents),
                        ),
                        author: id(author),
                        range,
                        pushers: pushers.into_iter().map(id).collect(),
                    },
                    commits,
                    clock,
                    pushes: Vec::new(),
                    approvals,
                    commands,
                }
            },
        )
}

/// What the writer writes of `state`.
fn written(state: &GitHubState) -> Value {
    serde_json::to_value(StateRecord::of(state)).expect("the record encodes")
}

proptest! {
    #[test]
    fn every_written_snapshot_reads_back_as_the_facts_it_was_written_from(state in snapshot()) {
        let value = written(&state);

        let recorded = RecordedState::read(&value);

        let recorded = recorded.expect("the writer's own record reads");
        prop_assert_eq!(recorded.head(), &state.head);
        prop_assert_eq!(recorded.lists(), &state.rule.lists);
        prop_assert_eq!(recorded.author(), state.rule.author);
        prop_assert_eq!(&recorded.range, &state.rule.range);
        prop_assert_eq!(recorded.commits(), state.commits);
        prop_assert_eq!(recorded.pushers(), &state.rule.pushers);
        prop_assert_eq!(recorded.push_clock(), state.clock);
    }

    #[test]
    fn the_ignored_approvals_read_back_as_the_snapshot_gave_them(state in snapshot()) {
        let value = written(&state);

        let record: StateRecordIn = serde_json::from_value(value).expect("the record reads");

        let expected: Vec<_> = state.ignored_approvals().collect();
        prop_assert_eq!(record.ignored_approvals(), Some(expected));
    }
}

/// A snapshot holding one approval and one command of each status with a reason.
fn fixed() -> GitHubState {
    GitHubState {
        head: commit('b'),
        rule: Rule {
            lists: lists(&[1], &[2], &[3], &[4]),
            author: id(9),
            range: RangeFacts::Attributed {
                users: [id(9)].into_iter().collect(),
                unattributed: false,
            },
            pushers: [id(8)].into_iter().collect(),
        },
        commits: CommitRange::Within,
        clock: Some("2026-10-13T09:00:00Z".parse().expect("a time")),
        pushes: Vec::new(),
        approvals: vec![Approval {
            review: NonZeroU64::MIN,
            reviewer: id(2),
            submitted_at: Some("2026-10-13T09:05:00Z".parse().expect("a time")),
            status: ApprovalStatus::Ineligible(Ineligible::Pusher),
        }],
        commands: vec![JudgedCommand {
            record: SnapshotCommand {
                comment_id: CommentId(NonZeroU64::MIN),
                actor: id(1),
                created_at: "2026-10-13T09:06:00Z".parse().expect("a time"),
                head_sha: commit('b'),
                body_sha256: digest('c'),
                command: Command::Waive { finding: fp() },
            },
            status: CommandStatus::Void(VoidCause::Edited),
        }],
    }
}

/// An edit: the object it changes (a JSON pointer), the key, and the value the key then holds
/// (`None` removes it).
type Edit = (&'static str, &'static str, Option<Value>);

#[test]
fn the_fixed_snapshot_reads_before_any_edit() {
    let recorded = RecordedState::read(&written(&fixed())).expect("the writer's record reads");

    assert_eq!(recorded.pushers(), &[id(8)].into_iter().collect());
}

#[test]
fn a_record_the_writer_never_writes_is_refused() {
    for (pointer, key, edit) in refused_edits() {
        let case = format!("{pointer}/{key} = {edit:?}");
        let mut value = written(&fixed());
        let object = value
            .pointer_mut(pointer)
            .and_then(Value::as_object_mut)
            .expect("the edited part is an object");
        match edit {
            Some(replacement) => object.insert(key.to_owned(), replacement),
            None => object.remove(key),
        };

        assert!(RecordedState::read(&value).is_none(), "{case}");
    }
}

/// Each edit that makes [`fixed`]'s record one the writer never writes: an unknown field in
/// every object, an unknown status or code, a reason where none is written or none where one
/// is, a command line the ledger's reader refuses (another word, `recheck`, arguments that do
/// not parse), an id that is zero and a time that is none.
fn refused_edits() -> [Edit; 18] {
    let null = || Some(Value::Null);
    let text = |text: &str| Some(Value::from(text));
    [
        ("", "extra", null()),
        ("/lists", "extra", null()),
        ("/lists/owners/0", "extra", null()),
        ("/range", "extra", null()),
        ("/range", "status", text("partial")),
        ("/approvals/0", "extra", null()),
        ("/approvals/0", "status", text("approved")),
        ("/approvals/0", "reason", text("banned")),
        ("/approvals/0", "status", text("counted")),
        ("/commands/0", "extra", null()),
        ("/commands/0", "status", text("applied")),
        ("/commands/0", "reason", text("gone")),
        ("/commands/0", "reason", None),
        ("/commands/0", "command", text("approve")),
        ("/commands/0", "command", text("recheck")),
        ("/commands/0", "args", Some(vec!["x"].into())),
        ("", "author", Some(0.into())),
        ("", "push_clock", text("yesterday")),
    ]
}
