//! `assay decide-only guard`'s rules (#61 DO-10, J3, 0006, D8 A) over fake GitHub reads: which
//! events name a PR to decide again, which are no-ops (`prs=[]`), which heads are refused, and
//! that a no-op reads nothing.

use std::cell::RefCell;
use std::error::Error;
use std::fmt;
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_app::{
    CheckName, CommitId, CompletedVerdict, ObservedConclusion, PollError, PollForge, PolledPull,
    Review, ReviewState, UserId, VerdictView,
};
use assay_cli::outputs::{DecideOnlyGuardOutputs, DecideOnlyPrs, DecideTrigger, PrsError};
use assay_cli::{
    DecideOnlyGuardJob, DecideOnlyGuardReads, GuardDecideOnlyError, GuardedDecideOnly, OutputName,
    Refusal, Skipped, UntrustedHead, guard_decide_only,
};
use assay_forge::github::{DecideOnlyEvent, DecideOnlyEventName, PullIdentity, PullState};
use serde_json::{Value, json};

/// The fixtures' repository, `Codertocat/Hello-World`.
const REPO_ID: u64 = 1_296_269;
const FORK_ID: u64 = 186_853_002;
const HEAD: &str = "ec26c3e57ca3a959ca5aad62de7213c562f8c821";
const BASE: &str = "f95f852bd8fca8fcc58a9a2d6c842781e32a215e";
/// The `assay-verdict` App's bot user in these tests.
const APP_BOT: u64 = 151_515;

fn id(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("non-zero")
}

fn sha(text: &str) -> CommitId {
    text.parse().expect("a full commit id")
}

fn job() -> DecideOnlyGuardJob {
    DecideOnlyGuardJob {
        repository_id: id(REPO_ID),
        app_bot: UserId(id(APP_BOT)),
    }
}

/// Forge's recorded decide-only payload `name`, changed by `edit`.
fn payload(
    event: DecideOnlyEventName,
    name: &str,
    edit: impl FnOnce(&mut Value),
) -> DecideOnlyEvent {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../assay-forge/tests/fixtures/github/events/decide_only")
        .join(name);
    let mut value: Value =
        serde_json::from_slice(&fs::read(&path).expect("a recorded payload")).expect("JSON");
    edit(&mut value);
    DecideOnlyEvent::read(event, value.to_string().as_bytes()).expect("a payload forge accepts")
}

fn comment(name: &str, edit: impl FnOnce(&mut Value)) -> DecideOnlyEvent {
    payload(DecideOnlyEventName::IssueComment, name, edit)
}

fn label(name: &str, edit: impl FnOnce(&mut Value)) -> DecideOnlyEvent {
    payload(DecideOnlyEventName::PullRequestTarget, name, edit)
}

fn schedule() -> DecideOnlyEvent {
    payload(DecideOnlyEventName::Schedule, "schedule.json", |_| {})
}

/// A read failure of the fake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadFailed;

impl fmt::Display for ReadFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("read failed")
    }
}

impl Error for ReadFailed {}

/// PR `n` as `pull_identity` reads it: open, its head in this repository.
fn open_pull(n: u64) -> PullIdentity {
    PullIdentity {
        number: id(n),
        head_sha: head(n),
        base_sha: sha(BASE),
        base_ref: "main".to_owned(),
        draft: false,
        author: UserId(id(21_031_067)),
        head_repo: Some(id(REPO_ID)),
        state: PullState::Open,
    }
}

/// PR `n`'s head: the fixtures' for PR 2, else `n` in hex.
fn head(n: u64) -> CommitId {
    if n == 2 {
        sha(HEAD)
    } else {
        sha(&format!("{n:040x}"))
    }
}

/// GitHub as the guard reads it: PRs by number, the poll's PRs waiting on an approval, and a
/// log of every read.
#[derive(Default)]
struct FakeReads {
    pulls: Vec<PullIdentity>,
    /// PRs whose verdict check is `failure (trust-path-unapproved)` at minute `.1`, approved
    /// on the head at minute 59.
    waiting: Vec<(u64, u32)>,
    /// Every read fails.
    fail: bool,
    /// Only the read logged exactly so fails (`open_pulls 50`, `pull 3`).
    fail_on: Option<&'static str>,
    calls: RefCell<Vec<String>>,
}

impl FakeReads {
    fn with_pull(mut self, pull: PullIdentity) -> Self {
        self.pulls.push(pull);
        self
    }

    fn waiting(mut self, n: u64, minute: u32) -> Self {
        self.waiting.push((n, minute));
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn log(&self, call: String) -> Result<(), ReadFailed> {
        let fails = self.fail || self.fail_on == Some(call.as_str());
        self.calls.borrow_mut().push(call);
        if fails { Err(ReadFailed) } else { Ok(()) }
    }
}

fn at(minute: u32) -> assay_app::Timestamp {
    format!("2026-10-31T10:{minute:02}:00Z")
        .parse()
        .expect("a UTC time")
}

impl PollForge for FakeReads {
    type Error = ReadFailed;

    fn open_pulls(&self, limit: usize) -> Result<Vec<PolledPull>, ReadFailed> {
        self.log(format!("open_pulls {limit}"))?;
        Ok(self
            .waiting
            .iter()
            .map(|&(n, _)| PolledPull {
                number: id(n),
                head_sha: head(n),
                base_ref: "main".to_owned(),
            })
            .collect())
    }

    fn verdict_check(
        &self,
        head: &CommitId,
        name: &CheckName,
    ) -> Result<Option<VerdictView>, ReadFailed> {
        assert_eq!(
            name.as_str(),
            "assay / verdict",
            "named for the default branch"
        );
        let minute = self
            .waiting
            .iter()
            .find(|&&(n, _)| self::head(n) == *head)
            .map(|&(_, minute)| minute)
            .expect("a listed head");
        Ok(Some(VerdictView::Completed(CompletedVerdict {
            conclusion: ObservedConclusion::Failure,
            title: Some("assay: failure (trust-path-unapproved)".to_owned()),
            completed_at: at(minute),
        })))
    }

    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, ReadFailed> {
        Ok(vec![Review {
            id: id(9_000),
            reviewer: Some(UserId(id(583_231))),
            state: ReviewState::Approved,
            commit: Some(head(number.get())),
            submitted_at: Some(at(59)),
        }])
    }
}

impl DecideOnlyGuardReads for FakeReads {
    fn pull_identity(&self, pr: NonZeroU64) -> Result<PullIdentity, ReadFailed> {
        self.log(format!("pull {pr}"))?;
        Ok(self
            .pulls
            .iter()
            .find(|pull| pull.number == pr)
            .cloned()
            .unwrap_or_else(|| open_pull(pr.get())))
    }

    fn default_branch(&self) -> Result<String, ReadFailed> {
        self.log("default_branch".to_owned())?;
        Ok("main".to_owned())
    }
}

/// The outputs of a run for `prs`.
fn run(prs: &[u64], trigger: DecideTrigger) -> GuardedDecideOnly {
    let prs =
        DecideOnlyPrs::try_from(prs.iter().copied().map(id).collect::<Vec<_>>()).expect("a PR set");
    GuardedDecideOnly::Run(DecideOnlyGuardOutputs::new(prs, trigger))
}

#[test]
fn a_users_recheck_on_a_pr_runs_for_that_pr() {
    let reads = FakeReads::default();

    let guarded = guard_decide_only(job(), &comment("comment_created_pr.json", |_| {}), &reads);

    assert_eq!(guarded, Ok(run(&[2], DecideTrigger::Comment)));
    assert_eq!(reads.calls(), ["pull 2"]);
}

#[test]
fn any_command_line_runs_even_one_the_grammar_refuses() {
    // The redecide job answers an unknown or malformed command (D10); only a line that is not
    // an `/assay` line at all is filtered out here.
    for body in ["/assay approve", "/assay waive nope", "/assay\nthanks"] {
        let reads = FakeReads::default();

        let guarded = guard_decide_only(
            job(),
            &comment("comment_created_pr.json", |v| {
                v["comment"]["body"] = json!(body);
            }),
            &reads,
        );

        assert_eq!(guarded, Ok(run(&[2], DecideTrigger::Comment)), "{body:?}");
    }
}

#[test]
fn a_comment_that_is_no_command_for_a_pr_is_a_no_op_that_reads_nothing() {
    let cases = [
        (
            comment("comment_created_pr.json", |v| {
                v["sender"]["type"] = json!("Bot");
                v["sender"]["id"] = json!(41_898_282);
                v["comment"]["user"]["id"] = json!(41_898_282);
            }),
            Skipped::Bot,
        ),
        (
            comment("comment_created_pr.json", |v| {
                v["sender"]["id"] = json!(APP_BOT);
                v["comment"]["user"]["id"] = json!(APP_BOT);
            }),
            Skipped::AppSender,
        ),
        (
            comment("comment_created_issue.json", |_| {}),
            Skipped::NotAPullRequest,
        ),
        (comment("comment_edited.json", |_| {}), Skipped::NotCreated),
        (comment("comment_deleted.json", |_| {}), Skipped::NotCreated),
        (
            comment("comment_created_pr.json", |v| {
                v["comment"]["body"] = json!("thanks\n/assay recheck");
            }),
            Skipped::NotACommand,
        ),
        (
            comment("comment_created_pr.json", |v| {
                v["comment"]["body"] = json!("> /assay recheck");
            }),
            Skipped::NotACommand,
        ),
    ];
    for (event, skipped) in cases {
        let reads = FakeReads::default();

        let guarded = guard_decide_only(job(), &event, &reads);

        assert_eq!(guarded, Ok(GuardedDecideOnly::Skip(skipped)), "{skipped:?}");
        assert_eq!(reads.calls(), [""; 0], "{skipped:?}");
    }
}

#[test]
fn a_skip_writes_only_an_empty_prs() {
    let outputs = GuardedDecideOnly::Skip(Skipped::Bot).outputs();

    assert_eq!(outputs, [(OutputName::Prs, "[]".to_owned())]);
}

#[test]
fn a_run_writes_prs_then_trigger_and_a_refusal_nothing() {
    let refused = GuardedDecideOnly::Refused(Refusal::UntrustedHead {
        pr: id(2),
        head: UntrustedHead::Deleted,
    });

    assert_eq!(
        run(&[2, 5], DecideTrigger::Schedule).outputs(),
        [
            (OutputName::Prs, "[2,5]".to_owned()),
            (OutputName::Trigger, "schedule".to_owned())
        ]
    );
    assert_eq!(refused.outputs(), []);
}

#[test]
fn a_command_on_a_fork_or_deleted_head_is_refused() {
    for (head_repo, head) in [
        (Some(id(FORK_ID)), UntrustedHead::Other(id(FORK_ID))),
        (None, UntrustedHead::Deleted),
    ] {
        let reads = FakeReads::default().with_pull(PullIdentity {
            head_repo,
            ..open_pull(2)
        });

        let guarded = guard_decide_only(job(), &comment("comment_created_pr.json", |_| {}), &reads);

        assert_eq!(
            guarded,
            Ok(GuardedDecideOnly::Refused(Refusal::UntrustedHead {
                pr: id(2),
                head
            }))
        );
    }
}

#[test]
fn a_command_on_a_closed_pr_is_a_no_op() {
    let reads = FakeReads::default().with_pull(PullIdentity {
        state: PullState::Closed,
        ..open_pull(2)
    });

    let guarded = guard_decide_only(job(), &comment("comment_created_pr.json", |_| {}), &reads);

    assert_eq!(guarded, Ok(GuardedDecideOnly::Skip(Skipped::Closed)));
}

#[test]
fn a_command_whose_pr_cannot_be_read_cannot_decide() {
    let reads = FakeReads {
        fail: true,
        ..FakeReads::default()
    };

    let guarded = guard_decide_only(job(), &comment("comment_created_pr.json", |_| {}), &reads);

    assert_eq!(
        guarded,
        Err(GuardDecideOnlyError::Pull {
            pr: id(2),
            source: ReadFailed
        })
    );
}

#[test]
fn a_users_assay_label_runs_for_its_pr_without_a_read() {
    for action in ["labeled", "unlabeled"] {
        let reads = FakeReads::default();

        let guarded = guard_decide_only(
            job(),
            &label("label_labeled_user.json", |v| v["action"] = json!(action)),
            &reads,
        );

        assert_eq!(guarded, Ok(run(&[2], DecideTrigger::Label)), "{action}");
        assert_eq!(reads.calls(), [""; 0], "{action}");
    }
}

#[test]
fn a_label_that_is_not_assay_s_or_that_a_bot_or_the_app_changed_is_a_no_op() {
    let cases = [
        (
            label("label_labeled_user.json", |v| {
                v["label"]["name"] = json!("bug");
            }),
            Skipped::NotAssayLabel,
        ),
        (
            label("label_labeled_user.json", |v| {
                v["label"]["name"] = json!("Assay:needs-owner");
            }),
            Skipped::NotAssayLabel,
        ),
        (
            label("label_labeled_user.json", |v| {
                v["label"]["name"] = json!(format!("assay:{}", "x".repeat(64)));
            }),
            Skipped::NotAssayLabel,
        ),
        (label("label_unlabeled_bot.json", |_| {}), Skipped::Bot),
        (
            label("label_labeled_user.json", |v| {
                v["sender"]["id"] = json!(APP_BOT);
            }),
            Skipped::AppSender,
        ),
    ];
    for (event, skipped) in cases {
        let reads = FakeReads::default();

        let guarded = guard_decide_only(job(), &event, &reads);

        assert_eq!(guarded, Ok(GuardedDecideOnly::Skip(skipped)), "{skipped:?}");
        assert_eq!(reads.calls(), [""; 0], "{skipped:?}");
    }
}

#[test]
fn a_label_on_a_fork_or_deleted_head_is_refused_from_the_payload_whatever_the_label() {
    for (repo, head) in [
        (json!({"id": FORK_ID}), UntrustedHead::Other(id(FORK_ID))),
        (Value::Null, UntrustedHead::Deleted),
    ] {
        for name in ["assay:needs-owner", "bug"] {
            let reads = FakeReads::default();
            let event = label("label_labeled_user.json", |v| {
                v["pull_request"]["head"]["repo"] = repo.clone();
                v["label"]["name"] = json!(name);
            });

            let guarded = guard_decide_only(job(), &event, &reads);

            assert_eq!(
                guarded,
                Ok(GuardedDecideOnly::Refused(Refusal::UntrustedHead {
                    pr: id(2),
                    head
                })),
                "{name}"
            );
            assert_eq!(reads.calls(), [""; 0]);
        }
    }
}

#[test]
fn a_payload_for_another_repository_cannot_decide_and_reads_nothing() {
    let events = [
        comment("comment_created_pr.json", |_| {}),
        comment("comment_created_issue.json", |_| {}),
        label("label_labeled_user.json", |_| {}),
        schedule(),
    ];
    for event in events {
        let reads = FakeReads::default();
        let other = DecideOnlyGuardJob {
            repository_id: id(7),
            ..job()
        };

        let guarded = guard_decide_only(other, &event, &reads);

        assert_eq!(
            guarded,
            Err(GuardDecideOnlyError::OtherRepository {
                payload: id(REPO_ID)
            })
        );
        assert_eq!(reads.calls(), [""; 0]);
    }
}

#[test]
fn the_poll_runs_for_the_waiting_prs_oldest_check_first() {
    let reads = FakeReads::default().waiting(4, 30).waiting(3, 10);

    let guarded = guard_decide_only(job(), &schedule(), &reads);

    assert_eq!(guarded, Ok(run(&[3, 4], DecideTrigger::Schedule)));
    assert_eq!(
        reads.calls(),
        ["default_branch", "open_pulls 50", "pull 3", "pull 4"]
    );
}

#[test]
fn the_poll_leaves_out_a_fork_deleted_or_closed_pr_and_runs_for_the_rest() {
    let reads = FakeReads::default()
        .waiting(3, 10)
        .waiting(4, 11)
        .waiting(5, 12)
        .waiting(6, 13)
        .with_pull(PullIdentity {
            head_repo: Some(id(FORK_ID)),
            ..open_pull(3)
        })
        .with_pull(PullIdentity {
            head_repo: None,
            ..open_pull(4)
        })
        .with_pull(PullIdentity {
            state: PullState::Closed,
            ..open_pull(6)
        });

    let guarded = guard_decide_only(job(), &schedule(), &reads);

    assert_eq!(guarded, Ok(run(&[5], DecideTrigger::Schedule)));
}

#[test]
fn a_poll_with_nothing_waiting_is_a_no_op() {
    let reads = FakeReads::default();
    let forks = FakeReads::default().waiting(3, 10).with_pull(PullIdentity {
        head_repo: None,
        ..open_pull(3)
    });

    assert_eq!(
        guard_decide_only(job(), &schedule(), &reads),
        Ok(GuardedDecideOnly::Skip(Skipped::NothingPending))
    );
    assert_eq!(
        guard_decide_only(job(), &schedule(), &forks),
        Ok(GuardedDecideOnly::Skip(Skipped::NothingPending))
    );
}

#[test]
fn a_poll_whose_reads_fail_cannot_decide() {
    let reads = FakeReads {
        fail: true,
        ..FakeReads::default()
    };

    let guarded = guard_decide_only(job(), &schedule(), &reads);

    assert_eq!(
        guarded,
        Err(GuardDecideOnlyError::DefaultBranch(ReadFailed))
    );
}

#[test]
fn a_poll_whose_listing_fails_is_a_poll_error() {
    let reads = FakeReads {
        fail_on: Some("open_pulls 50"),
        ..FakeReads::default().waiting(3, 10)
    };

    let guarded = guard_decide_only(job(), &schedule(), &reads);

    assert_eq!(
        guarded,
        Err(GuardDecideOnlyError::Poll(PollError::Pulls(ReadFailed)))
    );
    let error = guarded.expect_err("a poll error");
    assert_eq!(error.to_string(), "the hourly poll could not read GitHub");
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some("listing the open PRs failed")
    );
}

#[test]
fn a_poll_whose_read_of_a_selected_pr_fails_cannot_decide() {
    let reads = FakeReads {
        fail_on: Some("pull 4"),
        ..FakeReads::default().waiting(3, 10).waiting(4, 11)
    };

    let guarded = guard_decide_only(job(), &schedule(), &reads);

    assert_eq!(
        guarded,
        Err(GuardDecideOnlyError::Pull {
            pr: id(4),
            source: ReadFailed
        })
    );
}

#[test]
fn a_poll_selection_that_is_no_pr_set_is_an_error_not_a_no_op() {
    // A listing that names PR 3 twice: the poll selects it twice, which no run may get.
    let reads = FakeReads::default().waiting(3, 10).waiting(3, 10);

    let guarded = guard_decide_only(job(), &schedule(), &reads);

    assert_eq!(
        guarded,
        Err(GuardDecideOnlyError::Selection(PrsError::Repeated {
            pr: id(3)
        }))
    );
    let error = guarded.expect_err("a selection error");
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some("PR #3 is named twice")
    );
}
