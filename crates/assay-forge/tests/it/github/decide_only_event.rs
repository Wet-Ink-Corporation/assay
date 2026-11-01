//! `DecideOnlyEvent::read` over `issue_comment`, `pull_request_target` label and `schedule`
//! payloads (#61 DO-3).
//!
//! The fixtures under `tests/fixtures/github/events/decide_only/` are doc-derived, like the
//! pipeline's: written from the shapes GitHub's webhook reference documents for the
//! `issue_comment` and `pull_request` payloads (its example ids and names), with the `comment`
//! objects in the shape of this repository's recorded `issue_comment.json`. GitHub documents no
//! payload for `schedule`; its fixture is the repository and cron an Actions run's `event.json`
//! carries. None is recorded from a run: a runner's `event.json` cannot be fetched afterwards.

use std::assert_matches;
use std::io;
use std::num::NonZeroU64;

use assay_app::{
    CommentAction, CommentEvent, CommentId, CommitId, IssueComment, MAX_COMMENT_BYTES, Timestamp,
    UserId,
};
use assay_forge::github::{
    CommentPayload, DecideOnlyEvent, DecideOnlyEventError, DecideOnlyEventName, HeadRepository,
    Label, LabelAction, LabelName, LabelNameError, LabelPayload, MAX_EVENT_BYTES, MAX_LABEL_BYTES,
    Sender, SenderKind,
};

use super::recorded::fixture;

/// `octocat`, the documented payloads' sender and commenter.
const OCTOCAT: u64 = 583_231;
/// `github-actions[bot]`, a Bot sender.
const ACTIONS_BOT: u64 = 41_898_282;
/// `Codertocat/Hello-World`, the documented payloads' repository.
const REPOSITORY: u64 = 1_296_269;
/// The documented comment.
const COMMENT: u64 = 492_700_400;
/// The documented PR's head.
const HEAD: &str = "ec26c3e57ca3a959ca5aad62de7213c562f8c821";

fn id(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a positive test id")
}

fn time(text: &str) -> Timestamp {
    text.parse().expect("an RFC 3339 UTC time")
}

fn commit(text: &str) -> CommitId {
    text.parse().expect("a full commit id")
}

fn path(name: &str) -> String {
    format!("events/decide_only/{name}")
}

fn read(event: DecideOnlyEventName, name: &str) -> Result<DecideOnlyEvent, DecideOnlyEventError> {
    DecideOnlyEvent::read(event, fixture(&path(name)).as_slice())
}

fn comment(name: &str) -> Result<DecideOnlyEvent, DecideOnlyEventError> {
    read(DecideOnlyEventName::IssueComment, name)
}

fn label(name: &str) -> Result<DecideOnlyEvent, DecideOnlyEventError> {
    read(DecideOnlyEventName::PullRequestTarget, name)
}

/// Fixture `name` after `edit`.
fn fixture_with(name: &str, edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(&fixture(&path(name))).expect("JSON");
    edit(&mut value);
    serde_json::to_vec(&value).expect("JSON")
}

/// Fixture `name` after `edit`, read as `event`.
fn read_edited(
    event: DecideOnlyEventName,
    name: &str,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<DecideOnlyEvent, DecideOnlyEventError> {
    DecideOnlyEvent::read(event, fixture_with(name, edit).as_slice())
}

fn sender(value: u64, kind: SenderKind) -> Sender {
    Sender {
        id: UserId(id(value)),
        kind,
    }
}

/// The documented comment, `/assay recheck` by `octocat`, as `action` on `pr`.
fn octocat_comment(action: CommentAction, pr: Option<NonZeroU64>) -> CommentPayload {
    CommentPayload {
        repository_id: id(REPOSITORY),
        pr,
        sender: sender(OCTOCAT, SenderKind::User),
        event: CommentEvent {
            action,
            comment: IssueComment {
                id: CommentId(id(COMMENT)),
                user: Some(UserId(id(OCTOCAT))),
                created_at: time("2019-05-15T15:20:21Z"),
                updated_at: time("2019-05-15T15:20:21Z"),
                body: "/assay recheck".to_owned(),
                via_app: false,
            },
        },
    }
}

/// The documented PR 2's label event, `action` of `name` by `sender`.
fn label_event(action: LabelAction, name: &str, by: Sender) -> DecideOnlyEvent {
    DecideOnlyEvent::Label(label_payload(action, name, by))
}

/// The documented PR 2's label payload, `action` of `name` by `sender`.
fn label_payload(action: LabelAction, name: &str, by: Sender) -> LabelPayload {
    LabelPayload {
        action,
        label: Label::Name(LabelName::try_from(name.to_owned()).expect("a short label name")),
        number: id(2),
        head_sha: commit(HEAD),
        head_repository: HeadRepository::Base,
        base_ref: "main".to_owned(),
        sender: by,
        repository_id: id(REPOSITORY),
    }
}

#[test]
fn reads_a_comment_created_on_a_pr() {
    let read = comment("comment_created_pr.json").expect("a documented issue_comment payload");

    assert_eq!(
        read,
        DecideOnlyEvent::Comment(octocat_comment(CommentAction::Created, Some(id(2))))
    );
}

#[test]
fn reads_a_comment_on_an_issue_as_no_pr() {
    let read = comment("comment_created_issue.json").expect("a documented issue comment");

    assert_eq!(
        read,
        DecideOnlyEvent::Comment(octocat_comment(CommentAction::Created, None))
    );
}

#[test]
fn reads_an_edited_comment_as_edited() {
    let read = comment("comment_edited.json").expect("a documented edited payload");

    let mut expected = octocat_comment(CommentAction::Edited, Some(id(2)));
    expected.event.comment.body = "/assay dismiss ab12 the gate is flaky".to_owned();
    expected.event.comment.updated_at = time("2019-05-15T15:22:10Z");
    assert_eq!(read, DecideOnlyEvent::Comment(expected));
}

#[test]
fn reads_a_deleted_comment_as_deleted() {
    let read = comment("comment_deleted.json").expect("a documented deleted payload");

    assert_eq!(
        read,
        DecideOnlyEvent::Comment(octocat_comment(CommentAction::Deleted, Some(id(2))))
    );
}

#[test]
fn reads_a_comment_an_app_posted_as_via_app() {
    let read = comment("comment_created_via_app.json").expect("a comment an app posted");

    assert_eq!(
        read,
        DecideOnlyEvent::Comment(CommentPayload {
            repository_id: id(REPOSITORY),
            pr: Some(id(2)),
            sender: sender(ACTIONS_BOT, SenderKind::Bot),
            event: CommentEvent {
                action: CommentAction::Created,
                comment: IssueComment {
                    id: CommentId(id(492_700_401)),
                    user: Some(UserId(id(ACTIONS_BOT))),
                    created_at: time("2019-05-15T15:20:21Z"),
                    updated_at: time("2019-05-15T15:20:21Z"),
                    body: "<!-- temper-report -->\n### temper compliance: clean\n".to_owned(),
                    via_app: true,
                },
            },
        })
    );
}

#[test]
fn refuses_a_comment_that_does_not_say_whether_an_app_posted_it() {
    let read = read_edited(
        DecideOnlyEventName::IssueComment,
        "comment_created_pr.json",
        |v| {
            v["comment"]
                .as_object_mut()
                .expect("an object")
                .remove("performed_via_github_app");
        },
    );

    assert_matches!(
        read,
        Err(DecideOnlyEventError::Field(
            "comment.performed_via_github_app"
        ))
    );
}

#[test]
fn refuses_an_app_that_is_not_an_app_object() {
    let read = read_edited(
        DecideOnlyEventName::IssueComment,
        "comment_created_pr.json",
        |v| v["comment"]["performed_via_github_app"] = "github-actions".into(),
    );

    assert_matches!(read, Err(DecideOnlyEventError::Shape(_)));
}

#[test]
fn reads_a_comment_by_a_deleted_account_as_no_author() {
    let read = read_edited(
        DecideOnlyEventName::IssueComment,
        "comment_created_pr.json",
        |v| v["comment"]["user"] = serde_json::Value::Null,
    );

    let mut expected = octocat_comment(CommentAction::Created, Some(id(2)));
    expected.event.comment.user = None;
    assert_eq!(
        read.expect("a comment by a ghost"),
        DecideOnlyEvent::Comment(expected)
    );
}

#[test]
fn reads_a_label_a_user_added() {
    let read = label("label_labeled_user.json").expect("a documented labeled payload");

    assert_eq!(
        read,
        label_event(
            LabelAction::Labeled,
            "assay:needs-owner",
            sender(OCTOCAT, SenderKind::User)
        )
    );
}

#[test]
fn reads_a_label_a_bot_removed() {
    let read = label("label_unlabeled_bot.json").expect("a documented unlabeled payload");

    assert_eq!(
        read,
        label_event(
            LabelAction::Unlabeled,
            "assay:needs-owner",
            sender(ACTIONS_BOT, SenderKind::Bot)
        )
    );
}

#[test]
fn reads_a_schedule_as_its_repository() {
    let read = read(DecideOnlyEventName::Schedule, "schedule.json").expect("a schedule payload");

    assert_eq!(
        read,
        DecideOnlyEvent::Schedule {
            repository_id: id(REPOSITORY)
        }
    );
}

#[test]
fn reads_an_organization_sender() {
    let read = read_edited(
        DecideOnlyEventName::PullRequestTarget,
        "label_labeled_user.json",
        |v| v["sender"]["type"] = "Organization".into(),
    );

    assert_eq!(
        read.expect("an organization sender"),
        label_event(
            LabelAction::Labeled,
            "assay:needs-owner",
            sender(OCTOCAT, SenderKind::Organization)
        )
    );
}

#[test]
fn refuses_a_sender_type_outside_the_closed_set_shown_cut_to_64_bytes() {
    for (kind, shown) in [
        ("Mannequin".to_owned(), "Mannequin".to_owned()),
        ("user".to_owned(), "user".to_owned()),
        ("x".repeat(100), "x".repeat(64)),
    ] {
        for (event, name) in [
            (DecideOnlyEventName::IssueComment, "comment_created_pr.json"),
            (
                DecideOnlyEventName::PullRequestTarget,
                "label_labeled_user.json",
            ),
        ] {
            let read = read_edited(event, name, |v| v["sender"]["type"] = kind.as_str().into());

            assert_matches!(
                read,
                Err(DecideOnlyEventError::Sender(read)) if read == shown,
                "{name}: {kind}"
            );
        }
    }
}

/// The labeled fixture with its label named `name`.
fn labeled(name: &str) -> Result<DecideOnlyEvent, DecideOnlyEventError> {
    read_edited(
        DecideOnlyEventName::PullRequestTarget,
        "label_labeled_user.json",
        |v| v["label"]["name"] = name.into(),
    )
}

#[test]
fn reads_a_label_name_of_64_bytes_as_a_name() {
    let name = format!("assay:{}", "x".repeat(MAX_LABEL_BYTES - 6));

    let read = labeled(&name).expect("a name at the cap");

    assert_eq!(
        read,
        label_event(
            LabelAction::Labeled,
            &name,
            sender(OCTOCAT, SenderKind::User)
        )
    );
}

#[test]
fn reports_a_label_name_over_64_bytes_without_its_text() {
    let name = format!("assay:{}", "x".repeat(MAX_LABEL_BYTES - 5));

    let read = labeled(&name).expect("an over-long name is reported, not refused");

    assert_matches!(
        read,
        DecideOnlyEvent::Label(LabelPayload {
            label: Label::OverCap { bytes: 65 },
            ..
        })
    );
}

#[test]
fn refuses_an_empty_label_name_or_one_with_a_control_character() {
    for name in ["", "assay:needs\nowner"] {
        assert_matches!(
            labeled(name),
            Err(DecideOnlyEventError::Field("label.name")),
            "{name:?}"
        );
    }
}

#[test]
fn a_label_name_is_checked_on_construction() {
    let names = [
        "assay:needs-owner".to_owned(),
        String::new(),
        "assay:\u{7}".to_owned(),
        "é".repeat(33),
    ]
    .map(|name| LabelName::try_from(name).map(|name| name.as_str().to_owned()));

    assert_eq!(
        names,
        [
            Ok("assay:needs-owner".to_owned()),
            Err(LabelNameError::Empty),
            Err(LabelNameError::Control),
            Err(LabelNameError::OverCap { bytes: 66 }),
        ]
    );
}

#[test]
fn reports_a_label_on_a_fork_head_and_on_a_deleted_head() {
    let fork = read_edited(
        DecideOnlyEventName::PullRequestTarget,
        "label_labeled_user.json",
        |v| v["pull_request"]["head"]["repo"]["id"] = 186_853_002.into(),
    );
    let deleted = read_edited(
        DecideOnlyEventName::PullRequestTarget,
        "label_labeled_user.json",
        |v| v["pull_request"]["head"]["repo"] = serde_json::Value::Null,
    );

    let on = |head_repository| {
        DecideOnlyEvent::Label(LabelPayload {
            head_repository,
            ..label_payload(
                LabelAction::Labeled,
                "assay:needs-owner",
                sender(OCTOCAT, SenderKind::User),
            )
        })
    };
    assert_eq!(
        (fork.expect("a fork head"), deleted.expect("a deleted head")),
        (
            on(HeadRepository::Other(id(186_853_002))),
            on(HeadRepository::Deleted)
        )
    );
}

#[test]
fn refuses_an_action_it_does_not_read_by_name() {
    let pinned = read_edited(
        DecideOnlyEventName::IssueComment,
        "comment_created_pr.json",
        |v| v["action"] = "pinned".into(),
    );
    let opened = read_edited(
        DecideOnlyEventName::PullRequestTarget,
        "label_labeled_user.json",
        |v| v["action"] = "opened".into(),
    );
    let long = read_edited(
        DecideOnlyEventName::PullRequestTarget,
        "label_labeled_user.json",
        |v| v["action"] = "x".repeat(100).into(),
    );

    assert_matches!(pinned, Err(DecideOnlyEventError::Action(action)) if action == "pinned");
    assert_matches!(opened, Err(DecideOnlyEventError::Action(action)) if action == "opened");
    assert_matches!(long, Err(DecideOnlyEventError::Action(action)) if action == "x".repeat(64));
}

type Edit = fn(&mut serde_json::Value);

/// Each `(edit, path)` of fixture `name`, read as `event`, is refused as `path`.
fn assert_refused_by_path(event: DecideOnlyEventName, name: &str, cases: &[(Edit, &str)]) {
    for &(edit, field) in cases {
        let read = read_edited(event, name, edit);

        assert_matches!(
            read,
            Err(DecideOnlyEventError::Field(read)) if read == field,
            "{name}: {field}"
        );
    }
}

#[test]
fn refuses_a_comment_field_that_does_not_parse_by_its_path() {
    assert_refused_by_path(
        DecideOnlyEventName::IssueComment,
        "comment_created_pr.json",
        &[
            (|v| v["issue"]["number"] = 0.into(), "issue.number"),
            (
                |v| v["issue"]["pull_request"] = serde_json::Value::Null,
                "issue.pull_request",
            ),
            (|v| v["repository"]["id"] = 0.into(), "repository.id"),
            (|v| v["sender"]["id"] = 0.into(), "sender.id"),
            (|v| v["comment"]["id"] = 0.into(), "comment.id"),
            (|v| v["comment"]["user"]["id"] = 0.into(), "comment.user.id"),
            (
                |v| v["comment"]["created_at"] = "2019-05-15 15:20:21".into(),
                "comment.created_at",
            ),
            (
                |v| v["comment"]["updated_at"] = "yesterday".into(),
                "comment.updated_at",
            ),
        ],
    );
}

#[test]
fn refuses_a_label_field_that_does_not_parse_by_its_path() {
    assert_refused_by_path(
        DecideOnlyEventName::PullRequestTarget,
        "label_labeled_user.json",
        &[
            (|v| v["number"] = 3.into(), "number"),
            (|v| v["number"] = 0.into(), "number"),
            (|v| v["repository"]["id"] = 0.into(), "repository.id"),
            (|v| v["label"] = serde_json::Value::Null, "label"),
            (
                |v| {
                    v.as_object_mut().expect("an object").remove("label");
                },
                "label",
            ),
            (
                |v| v["pull_request"]["head"]["sha"] = "abc".into(),
                "pull_request.head.sha",
            ),
            (
                |v| v["pull_request"]["base"]["repo"] = serde_json::Value::Null,
                "pull_request.base.repo",
            ),
            (
                |v| v["pull_request"]["base"]["repo"]["id"] = 7.into(),
                "pull_request.base.repo.id",
            ),
            (
                |v| v["pull_request"]["head"]["repo"]["id"] = 0.into(),
                "pull_request.head.repo.id",
            ),
            (
                |v| v["pull_request"]["base"]["ref"] = "".into(),
                "pull_request.base.ref",
            ),
            (|v| v["sender"]["id"] = 0.into(), "sender.id"),
        ],
    );
}

#[test]
fn refuses_a_schedule_field_that_does_not_parse_by_its_path() {
    assert_refused_by_path(
        DecideOnlyEventName::Schedule,
        "schedule.json",
        &[
            (|v| v["schedule"] = " ".into(), "schedule"),
            (|v| v["repository"]["id"] = 0.into(), "repository.id"),
        ],
    );
}

#[test]
fn refuses_a_schedule_without_a_repository_or_a_cron() {
    for key in ["repository", "schedule"] {
        let read = read_edited(DecideOnlyEventName::Schedule, "schedule.json", |v| {
            v.as_object_mut().expect("an object").remove(key);
        });

        assert_matches!(read, Err(DecideOnlyEventError::Shape(_)), "{key}");
    }
}

#[test]
fn refuses_a_comment_body_over_the_comment_cap() {
    let at_cap = read_edited(
        DecideOnlyEventName::IssueComment,
        "comment_created_pr.json",
        |v| v["comment"]["body"] = "a".repeat(MAX_COMMENT_BYTES).into(),
    );
    let over = read_edited(
        DecideOnlyEventName::IssueComment,
        "comment_created_pr.json",
        |v| v["comment"]["body"] = "a".repeat(MAX_COMMENT_BYTES + 1).into(),
    );

    assert_matches!(at_cap, Ok(DecideOnlyEvent::Comment(_)));
    assert_matches!(over, Err(DecideOnlyEventError::Field("comment.body")));
}

#[test]
fn refuses_a_repeated_key() {
    let body = br#"{"action": "created", "action": "deleted"}"#;

    assert_matches!(
        DecideOnlyEvent::read(DecideOnlyEventName::IssueComment, &body[..]),
        Err(DecideOnlyEventError::Shape(_))
    );
}

#[test]
fn refuses_a_payload_over_the_cap_unparsed() {
    let mut body = fixture(&path("comment_created_pr.json"));
    body.resize(MAX_EVENT_BYTES + 1, b' ');

    assert_matches!(
        DecideOnlyEvent::read(DecideOnlyEventName::IssueComment, body.as_slice()),
        Err(DecideOnlyEventError::TooLarge {
            cap: MAX_EVENT_BYTES
        })
    );
}

#[test]
fn reads_a_payload_at_the_cap() {
    let mut body = fixture(&path("comment_created_pr.json"));
    body.resize(MAX_EVENT_BYTES, b' ');

    let read = DecideOnlyEvent::read(DecideOnlyEventName::IssueComment, body.as_slice())
        .expect("a payload at the cap");

    assert_eq!(
        read,
        DecideOnlyEvent::Comment(octocat_comment(CommentAction::Created, Some(id(2))))
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
        DecideOnlyEvent::read(DecideOnlyEventName::Schedule, Broken),
        Err(DecideOnlyEventError::Read(error)) if error.kind() == io::ErrorKind::BrokenPipe
    );
}

#[test]
fn refuses_a_pipeline_payload_read_as_a_comment() {
    let body = fixture("events/event_opened.json");

    assert_matches!(
        DecideOnlyEvent::read(DecideOnlyEventName::IssueComment, body.as_slice()),
        Err(DecideOnlyEventError::Shape(_))
    );
}

#[test]
fn reads_only_the_three_event_names() {
    let names = [
        "issue_comment",
        "pull_request_target",
        "schedule",
        "pull_request",
        "workflow_dispatch",
        "pull_request_review",
        "",
    ]
    .map(|name| name.parse::<DecideOnlyEventName>().ok());

    assert_eq!(
        names,
        [
            Some(DecideOnlyEventName::IssueComment),
            Some(DecideOnlyEventName::PullRequestTarget),
            Some(DecideOnlyEventName::Schedule),
            None,
            None,
            None,
            None
        ]
    );
}
