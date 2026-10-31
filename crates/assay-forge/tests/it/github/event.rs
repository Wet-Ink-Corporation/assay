//! `EventPayload::read` over `pull_request_target` and `workflow_dispatch` payloads (#60).
//!
//! The fixtures under `tests/fixtures/github/events/` are doc-derived: written from the shape
//! GitHub's webhook reference documents for the `pull_request` and `workflow_dispatch`
//! payloads (its example ids and names), not recorded from a run.

use std::assert_matches;
use std::io;
use std::num::NonZeroU64;

use assay_app::{CommitId, UserId};
use assay_forge::github::{
    DispatchPayload, EventName, EventPayload, EventPayloadError, HeadRepository, MAX_EVENT_BYTES,
    PullRequestAction, PullRequestPayload,
};

use super::recorded::fixture;

const HEAD: &str = "ec26c3e57ca3a959ca5aad62de7213c562f8c821";
const BASE: &str = "f95f852bd8fca8fcc58a9a2d6c842781e32a215e";
const BEFORE: &str = "9049f1265b7d61be4a8904a9a27120d2064dab3b";

fn id(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a positive test id")
}

fn commit(text: &str) -> CommitId {
    text.parse().expect("a full commit id")
}

fn read(event: EventName, name: &str) -> Result<EventPayload, EventPayloadError> {
    EventPayload::read(event, fixture(&format!("events/{name}")).as_slice())
}

fn pull(name: &str) -> Result<EventPayload, EventPayloadError> {
    read(EventName::PullRequestTarget, name)
}

/// The documented payload's PR, as `action` with `head` saw it.
fn payload(action: PullRequestAction, head_repository: HeadRepository) -> EventPayload {
    EventPayload::PullRequest(PullRequestPayload {
        action,
        number: id(2),
        head_sha: commit(HEAD),
        base_sha: commit(BASE),
        base_ref: "main".to_owned(),
        draft: false,
        author: UserId(id(21_031_067)),
        sender: UserId(id(583_231)),
        repository_id: id(1_296_269),
        head_repository,
        default_branch: "main".to_owned(),
    })
}

#[test]
fn reads_opened() {
    let read = pull("event_opened.json").expect("a documented opened payload");

    assert_eq!(
        read,
        payload(PullRequestAction::Opened, HeadRepository::Base)
    );
}

#[test]
fn reads_synchronize_with_before() {
    let read = pull("event_synchronize.json").expect("a documented synchronize payload");

    assert_eq!(
        read,
        payload(
            PullRequestAction::Synchronize {
                before: commit(BEFORE)
            },
            HeadRepository::Base
        )
    );
}

#[test]
fn reads_reopened_and_ready_for_review() {
    let reopened = pull("event_reopened.json").expect("a documented reopened payload");
    let ready = pull("event_ready_for_review.json").expect("a documented ready payload");

    assert_eq!(
        (reopened, ready),
        (
            payload(PullRequestAction::Reopened, HeadRepository::Base),
            payload(PullRequestAction::ReadyForReview, HeadRepository::Base)
        )
    );
}

#[test]
fn reads_edited_with_and_without_a_base_change() {
    let base = pull("event_edited_base.json").expect("a documented edited payload");
    let title = pull("event_edited_title.json").expect("a documented edited payload");

    assert_eq!(
        (base, title),
        (
            payload(
                PullRequestAction::Edited { base_changed: true },
                HeadRepository::Base
            ),
            payload(
                PullRequestAction::Edited {
                    base_changed: false
                },
                HeadRepository::Base
            )
        )
    );
}

#[test]
fn reports_a_head_in_another_repository() {
    let read = pull("event_opened_fork.json").expect("a documented fork payload");

    assert_eq!(
        read,
        payload(
            PullRequestAction::Opened,
            HeadRepository::Other(id(186_853_002))
        )
    );
}

#[test]
fn reports_a_deleted_head_repository() {
    let read = pull("event_synchronize_head_deleted.json").expect("a payload with no head repo");

    assert_eq!(
        read,
        payload(
            PullRequestAction::Synchronize {
                before: commit(BEFORE)
            },
            HeadRepository::Deleted
        )
    );
}

#[test]
fn refuses_labeled_by_name() {
    assert_matches!(
        pull("event_labeled.json"),
        Err(EventPayloadError::Action(action)) if action == "labeled"
    );
}

#[test]
fn reads_a_dispatch_naming_only_the_pr() {
    let read = read(EventName::WorkflowDispatch, "event_dispatch.json").expect("a dispatch");

    assert_eq!(
        read,
        EventPayload::Dispatch(DispatchPayload {
            pr: id(2),
            sender: UserId(id(583_231)),
            repository_id: id(1_296_269),
            default_branch: "main".to_owned(),
        })
    );
}

#[test]
fn refuses_a_dispatch_with_another_input() {
    assert_matches!(
        read(EventName::WorkflowDispatch, "event_dispatch_role.json"),
        Err(EventPayloadError::DispatchInput(input)) if input == "role"
    );
}

/// The `workflow_dispatch` fixture with `inputs` replaced by `inputs`.
fn dispatch_with(inputs: serde_json::Value) -> Vec<u8> {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fixture("events/event_dispatch.json")).expect("JSON");
    value["inputs"] = inputs;
    serde_json::to_vec(&value).expect("JSON")
}

#[test]
fn refuses_a_dispatch_whose_pr_is_missing_or_not_a_number() {
    for inputs in [
        serde_json::json!({}),
        serde_json::Value::Null,
        serde_json::json!({ "pr": "0" }),
        serde_json::json!({ "pr": "02" }),
        serde_json::json!({ "pr": "#2" }),
        serde_json::json!({ "pr": 2 }),
    ] {
        let body = dispatch_with(inputs.clone());

        assert_matches!(
            EventPayload::read(EventName::WorkflowDispatch, body.as_slice()),
            Err(EventPayloadError::Field("inputs.pr")),
            "{inputs}"
        );
    }
}

#[test]
fn refuses_a_pull_request_payload_read_as_a_dispatch() {
    assert_matches!(
        read(EventName::WorkflowDispatch, "event_opened.json"),
        Err(EventPayloadError::Field("inputs.pr"))
    );
}

/// The opened fixture after `edit`.
fn opened_with(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fixture("events/event_opened.json")).expect("JSON");
    edit(&mut value);
    serde_json::to_vec(&value).expect("JSON")
}

#[test]
fn refuses_a_field_that_does_not_parse_by_its_path() {
    type Edit = fn(&mut serde_json::Value);
    let cases: [(Edit, &str); 6] = [
        (|v| v["number"] = 3.into(), "number"),
        (
            |v| v["pull_request"]["head"]["sha"] = "abc".into(),
            "pull_request.head.sha",
        ),
        (
            |v| v["pull_request"]["base"]["repo"]["id"] = 7.into(),
            "pull_request.base.repo.id",
        ),
        (
            |v| v["pull_request"]["user"] = serde_json::Value::Null,
            "pull_request.user",
        ),
        (|v| v["sender"]["id"] = 0.into(), "sender.id"),
        (
            |v| v["pull_request"]["base"]["ref"] = "".into(),
            "pull_request.base.ref",
        ),
    ];
    for (edit, field) in cases {
        let body = opened_with(edit);

        assert_matches!(
            EventPayload::read(EventName::PullRequestTarget, body.as_slice()),
            Err(EventPayloadError::Field(read)) if read == field,
            "{field}"
        );
    }
}

#[test]
fn refuses_a_synchronize_whose_after_is_not_the_head() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fixture("events/event_synchronize.json")).expect("JSON");
    value["after"] = BEFORE.into();
    let body = serde_json::to_vec(&value).expect("JSON");

    assert_matches!(
        EventPayload::read(EventName::PullRequestTarget, body.as_slice()),
        Err(EventPayloadError::Field("after"))
    );
}

#[test]
fn refuses_a_synchronize_without_before() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fixture("events/event_synchronize.json")).expect("JSON");
    value.as_object_mut().expect("an object").remove("before");
    let body = serde_json::to_vec(&value).expect("JSON");

    assert_matches!(
        EventPayload::read(EventName::PullRequestTarget, body.as_slice()),
        Err(EventPayloadError::Field("before"))
    );
}

#[test]
fn refuses_a_missing_draft_rather_than_defaulting_it() {
    let body = opened_with(|v| {
        v["pull_request"]
            .as_object_mut()
            .expect("an object")
            .remove("draft");
    });

    assert_matches!(
        EventPayload::read(EventName::PullRequestTarget, body.as_slice()),
        Err(EventPayloadError::Shape(_))
    );
}

#[test]
fn refuses_a_repeated_key() {
    let body = br#"{"action": "opened", "action": "closed"}"#;

    assert_matches!(
        EventPayload::read(EventName::PullRequestTarget, &body[..]),
        Err(EventPayloadError::Shape(_))
    );
}

#[test]
fn refuses_a_payload_over_the_cap_unparsed() {
    let mut body = fixture("events/event_opened.json");
    body.resize(MAX_EVENT_BYTES + 1, b' ');

    assert_matches!(
        EventPayload::read(EventName::PullRequestTarget, body.as_slice()),
        Err(EventPayloadError::TooLarge {
            cap: MAX_EVENT_BYTES
        })
    );
}

#[test]
fn reads_a_payload_at_the_cap() {
    let mut body = fixture("events/event_opened.json");
    body.resize(MAX_EVENT_BYTES, b' ');

    let read = EventPayload::read(EventName::PullRequestTarget, body.as_slice())
        .expect("a payload at the cap");

    assert_eq!(
        read,
        payload(PullRequestAction::Opened, HeadRepository::Base)
    );
}

#[test]
fn reports_a_failed_read() {
    struct Broken;
    impl io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    assert_matches!(
        EventPayload::read(EventName::PullRequestTarget, Broken),
        Err(EventPayloadError::Read(error)) if error.kind() == io::ErrorKind::BrokenPipe
    );
}

#[test]
fn reads_only_the_two_event_names() {
    let names = [
        "pull_request_target",
        "workflow_dispatch",
        "pull_request",
        "push",
        "",
    ]
    .map(|name| name.parse::<EventName>().ok());

    assert_eq!(
        names,
        [
            Some(EventName::PullRequestTarget),
            Some(EventName::WorkflowDispatch),
            None,
            None,
            None
        ]
    );
}

#[test]
fn an_unknown_action_is_shown_cut_to_64_bytes() {
    let action = "x".repeat(100);
    let body = opened_with(|v| v["action"] = action.clone().into());

    assert_matches!(
        EventPayload::read(EventName::PullRequestTarget, body.as_slice()),
        Err(EventPayloadError::Action(shown)) if shown == "x".repeat(64)
    );
}
