//! `assay guard`'s rules (#60 ST-2, 0006) over a fake PR read: which events run, which are
//! filtered out (`run=false`), which heads are refused, and the identity each run gets.

use std::error::Error;
use std::fmt;
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;

use assay_app::{CommitId, UserId};
use assay_cli::outputs::{Identity, Trigger};
use assay_cli::{GuardError, Guarded, OutputName, Refusal, UntrustedHead, guard};
use assay_forge::github::{
    EventName, EventPayload, HeadRepository, PullIdentity, PullRequestPayload, PullState,
};

/// The fixtures' repository, `Codertocat/Hello-World`.
const REPO_ID: u64 = 1_296_269;
/// The fork in `event_opened_fork.json`.
const FORK_ID: u64 = 186_853_002;
const HEAD: &str = "ec26c3e57ca3a959ca5aad62de7213c562f8c821";
const BASE: &str = "f95f852bd8fca8fcc58a9a2d6c842781e32a215e";

/// `n` (at least 1) as a non-zero id.
fn id(n: u64) -> NonZeroU64 {
    NonZeroU64::MIN.saturating_add(n.saturating_sub(1))
}

fn sha(text: &str) -> CommitId {
    text.parse().expect("a full commit id")
}

/// Forge's recorded event payload `name`.
fn payload(event: EventName, name: &str) -> EventPayload {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../assay-forge/tests/fixtures/github/events")
        .join(name);
    let file = fs::File::open(&path).expect("a recorded payload");
    EventPayload::read(event, file).expect("a payload forge accepts")
}

fn pull_request(name: &str) -> EventPayload {
    payload(EventName::PullRequestTarget, name)
}

/// The read failure the fake PR read reports.
#[derive(Debug, PartialEq, Eq)]
struct ReadFailed;

impl fmt::Display for ReadFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("read failed")
    }
}

impl Error for ReadFailed {}

/// A PR read that must not happen.
fn no_read(number: NonZeroU64) -> Result<PullIdentity, ReadFailed> {
    panic!("PR #{number} was read for an event that carries it");
}

/// PR 2 as `pull_identity` reads it: open, its head in this repository.
fn open_pull() -> PullIdentity {
    PullIdentity {
        number: id(2),
        head_sha: sha(HEAD),
        base_sha: sha(BASE),
        base_ref: "main".to_owned(),
        draft: true,
        author: UserId(id(21_031_067)),
        head_repo: Some(id(REPO_ID)),
        state: PullState::Open,
    }
}

/// The identity of PR 2 in the fixtures, for `trigger`.
fn identity(trigger: Trigger) -> Identity {
    Identity {
        pr: id(2),
        head_sha: sha(HEAD),
        base_sha: sha(BASE),
        base_ref: "main".to_owned(),
        draft: false,
        author: UserId(id(21_031_067)),
        sender: UserId(id(583_231)),
        trigger,
        default_branch: "main".to_owned(),
    }
}

#[test]
fn a_same_repository_opened_pr_runs_with_its_identity() {
    let guarded = guard(id(REPO_ID), pull_request("event_opened.json"), no_read);

    assert_eq!(guarded, Ok(Guarded::Run(identity(Trigger::Opened))));
}

#[test]
fn a_synchronize_push_carries_the_head_before_it() {
    let guarded = guard(id(REPO_ID), pull_request("event_synchronize.json"), no_read);

    let Ok(Guarded::Run(identity)) = guarded else {
        panic!("a same-repository push runs: {guarded:?}");
    };
    assert_eq!(
        identity.trigger,
        Trigger::Synchronize {
            before: sha("9049f1265b7d61be4a8904a9a27120d2064dab3b")
        }
    );
}

#[test]
fn a_fork_head_is_refused_as_untrusted() {
    let guarded = guard(id(REPO_ID), pull_request("event_opened_fork.json"), no_read);

    assert_eq!(
        guarded,
        Ok(Guarded::Refused(Refusal::UntrustedHead {
            pr: id(2),
            head: UntrustedHead::Other(id(FORK_ID)),
        }))
    );
}

#[test]
fn a_deleted_head_repository_is_refused_as_untrusted() {
    let guarded = guard(
        id(REPO_ID),
        pull_request("event_synchronize_head_deleted.json"),
        no_read,
    );

    assert_eq!(
        guarded,
        Ok(Guarded::Refused(Refusal::UntrustedHead {
            pr: id(2),
            head: UntrustedHead::Deleted,
        }))
    );
}

#[test]
fn an_edit_without_a_base_change_does_not_run() {
    let guarded = guard(
        id(REPO_ID),
        pull_request("event_edited_title.json"),
        no_read,
    );

    assert_eq!(guarded, Ok(Guarded::Skip));
}

#[test]
fn an_untrusted_head_is_refused_before_the_edit_filter() {
    for (head_repository, head) in [
        (
            HeadRepository::Other(id(FORK_ID)),
            UntrustedHead::Other(id(FORK_ID)),
        ),
        (HeadRepository::Deleted, UntrustedHead::Deleted),
    ] {
        let EventPayload::PullRequest(edited) = pull_request("event_edited_title.json") else {
            panic!("a pull_request_target payload");
        };
        let event = EventPayload::PullRequest(PullRequestPayload {
            head_repository,
            ..edited
        });

        let guarded = guard(id(REPO_ID), event, no_read);

        assert_eq!(
            guarded,
            Ok(Guarded::Refused(Refusal::UntrustedHead { pr: id(2), head })),
            "{head:?}"
        );
    }
}

#[test]
fn an_edit_that_changes_the_base_runs() {
    let guarded = guard(id(REPO_ID), pull_request("event_edited_base.json"), no_read);

    let Ok(Guarded::Run(identity)) = guarded else {
        panic!("a base change runs: {guarded:?}");
    };
    assert_eq!(identity.trigger, Trigger::Edited);
}

#[test]
fn a_payload_for_another_repository_cannot_run() {
    for event in [
        pull_request("event_opened.json"),
        payload(EventName::WorkflowDispatch, "event_dispatch.json"),
    ] {
        let guarded = guard(id(7), event, no_read);

        assert_eq!(
            guarded,
            Err(GuardError::OtherRepository {
                payload: id(REPO_ID)
            })
        );
    }
}

#[test]
fn a_dispatch_runs_with_the_prs_identity_as_read() {
    let mut asked = None;

    let guarded = guard(
        id(REPO_ID),
        payload(EventName::WorkflowDispatch, "event_dispatch.json"),
        |number| {
            asked = Some(number);
            Ok::<_, ReadFailed>(open_pull())
        },
    );

    assert_eq!(asked, Some(id(2)));
    assert_eq!(
        guarded,
        Ok(Guarded::Run(Identity {
            draft: true,
            ..identity(Trigger::Dispatch)
        }))
    );
}

#[test]
fn a_dispatch_for_a_closed_pr_is_refused() {
    let guarded = guard(
        id(REPO_ID),
        payload(EventName::WorkflowDispatch, "event_dispatch.json"),
        |_| {
            Ok::<_, ReadFailed>(PullIdentity {
                state: PullState::Closed,
                ..open_pull()
            })
        },
    );

    assert_eq!(guarded, Ok(Guarded::Refused(Refusal::Closed { pr: id(2) })));
}

#[test]
fn a_dispatch_whose_head_is_not_this_repository_is_refused() {
    for (head_repo, head) in [
        (Some(id(FORK_ID)), UntrustedHead::Other(id(FORK_ID))),
        (None, UntrustedHead::Deleted),
    ] {
        let guarded = guard(
            id(REPO_ID),
            payload(EventName::WorkflowDispatch, "event_dispatch.json"),
            |_| {
                Ok::<_, ReadFailed>(PullIdentity {
                    head_repo,
                    ..open_pull()
                })
            },
        );

        assert_eq!(
            guarded,
            Ok(Guarded::Refused(Refusal::UntrustedHead { pr: id(2), head }))
        );
    }
}

#[test]
fn a_dispatch_whose_pr_cannot_be_read_cannot_run() {
    let guarded = guard(
        id(REPO_ID),
        payload(EventName::WorkflowDispatch, "event_dispatch.json"),
        |_| Err(ReadFailed),
    );

    assert_eq!(
        guarded,
        Err(GuardError::Pull {
            pr: id(2),
            source: ReadFailed
        })
    );
}

#[test]
fn a_dispatch_whose_base_branch_holds_a_control_character_cannot_run() {
    let guarded = guard(
        id(REPO_ID),
        payload(EventName::WorkflowDispatch, "event_dispatch.json"),
        |_| {
            Ok::<_, ReadFailed>(PullIdentity {
                base_ref: "main\nrun=true".to_owned(),
                ..open_pull()
            })
        },
    );

    assert_eq!(guarded, Err(GuardError::BaseRef { pr: id(2) }));
}

#[test]
fn a_run_writes_its_identity_and_a_skip_writes_only_run_false() {
    let run = Guarded::Run(identity(Trigger::Opened));

    assert_eq!(run.outputs(), identity(Trigger::Opened).outputs());
    assert_eq!(
        Guarded::Skip.outputs(),
        [(OutputName::Run, "false".to_owned())]
    );
    assert_eq!(
        Guarded::Refused(Refusal::Closed { pr: id(2) }).outputs(),
        []
    );
}

#[test]
fn an_untrusted_head_is_named_as_such() {
    let refusal = Refusal::UntrustedHead {
        pr: id(2),
        head: UntrustedHead::Other(id(FORK_ID)),
    };

    assert_eq!(
        refusal.to_string(),
        "untrusted head repository: PR #2's head is in repository 186853002, not this one; \
         assay does not run for it (0006)"
    );
}
