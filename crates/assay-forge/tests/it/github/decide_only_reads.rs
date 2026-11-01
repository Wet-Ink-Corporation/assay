//! decide-only's GitHub ports (#61 DO-9) over recorded responses: replies to command comments
//! (D10), the base tip's eligibility lists (D4) and the hourly poll's reads (D8).
//!
//! The reply listings are the recorded `issue_comments.json` with the App's comment's body
//! edited. The tip list files follow the contents API shape of the recorded
//! `contents_warm_target.json`; a directory answer is a list of entries (as
//! `contents_workflows.json`), and a symlink's is GitHub's documented `type: symlink` object, or
//! the target file's object when GitHub follows a symlink to a file of the repository.

use std::time::Duration;

use assay_app::{
    CheckName, CommandRejected, CommandReplies, CommentId, CompletedVerdict, Forge, MAX_POLL_PULLS,
    MAX_TIP_LIST_BYTES, ObservedConclusion, PollForge, PolledPull, Reply, TipListFiles, TipLists,
    VerdictView,
};
use assay_forge::github::{ForgeError, Method, Operation, Response, TipEntry, TipListsError};
use serde_json::{Value, json};

use super::contents::encode;
use super::recorded::{
    Recorded, Waits, fixture, fixture_json, github, head, id, json, path, pr, reply, write_bodies,
};

/// The command comment the replies answer.
fn command() -> CommentId {
    CommentId(id(6_255_521_953))
}

/// PR 268's issue comments.
fn listing() -> String {
    path("issues/268/comments?per_page=100")
}

/// The reply posted in these tests.
const REPLY: Reply = Reply::Rejected(CommandRejected::Unknown);

/// The recorded comments, the App's comment's body replaced by `body`.
fn comments_with_app_body(body: &str) -> Vec<u8> {
    let mut comments = fixture_json("issue_comments.json");
    let app = comments
        .as_array_mut()
        .and_then(|all| all.last_mut())
        .expect("the App's comment is recorded last");
    app["body"] = Value::from(body);
    json(&comments)
}

#[test]
fn a_reply_posts_its_marker_and_sentence() {
    let recorded = Recorded::default().then(
        Method::Post,
        &path("issues/268/comments"),
        reply(201, &[], json(&json!({}))),
    );
    let waits = Waits::default();

    let replied = github(&recorded, &waits).reply(pr(), command(), REPLY);

    assert_eq!(replied.expect("posted"), ());
    assert_eq!(
        write_bodies(&recorded),
        [Some(json!({
            "body": "<!-- assay-reply 6255521953 -->\nassay did not record this command: \
                     unknown command."
        }))]
    );
    assert_eq!(recorded.unused(), 0);
}

#[test]
fn a_reply_that_landed_before_a_5xx_is_not_posted_again() {
    let recorded = Recorded::default()
        .then(
            Method::Post,
            &path("issues/268/comments"),
            reply(502, &[], Vec::new()),
        )
        .ok(
            Method::Get,
            &listing(),
            comments_with_app_body(&REPLY.body(command())),
        );
    let waits = Waits::default();

    let replied = github(&recorded, &waits).reply(pr(), command(), REPLY);

    assert_eq!(replied.expect("landed"), ());
    assert_eq!(recorded.writes().len(), 1, "one POST only");
    assert_eq!(waits.taken(), [Duration::from_secs(1)]);
}

#[test]
fn a_reply_the_probe_does_not_find_is_posted_again() {
    let post = path("issues/268/comments");
    // The App's comment carries the marker under another sentence, and a user's carries the
    // whole body: neither is this reply.
    let mut comments = fixture_json("issue_comments.json");
    let all = comments.as_array_mut().expect("a listing");
    if let Some(user) = all.get_mut(1) {
        user["body"] = Value::from(REPLY.body(command()));
    }
    if let Some(app) = all.last_mut() {
        app["body"] = Value::from(Reply::marker(command()));
    }
    let recorded = Recorded::default()
        .then(Method::Post, &post, reply(502, &[], Vec::new()))
        .ok(Method::Get, &listing(), json(&comments))
        .then(Method::Post, &post, reply(201, &[], json(&json!({}))));
    let waits = Waits::default();

    let replied = github(&recorded, &waits).reply(pr(), command(), REPLY);

    assert_eq!(replied.expect("the second POST lands"), ());
    let body = Some(json!({ "body": REPLY.body(command()) }));
    assert_eq!(write_bodies(&recorded), [body.clone(), body]);
    assert_eq!(recorded.unused(), 0, "POST, probe, POST");
}

/// `contents/<file>?ref=<base>`.
fn contents(file: &str, base: &str) -> String {
    path(&format!("contents/{file}?ref={base}"))
}

/// The contents API's answer for `file` holding `bytes`, as the recorded file answer.
fn file_answer(file: &str, bytes: &[u8]) -> Value {
    let mut answer = fixture_json("contents_warm_target.json");
    answer["name"] = Value::from(file.rsplit('/').next());
    answer["path"] = Value::from(file);
    answer["size"] = Value::from(bytes.len());
    answer["content"] = Value::from(encode(bytes));
    answer
}

/// GitHub's 404 for a path it does not have.
fn absent() -> Response {
    reply(404, &[], fixture("not_found.json"))
}

const OWNERS: &[u8] = b"owners = [124330161]\napprovers = []\nmaintainers = []\n";
const AGENTS: &[u8] = b"agents = [255000001]\n";

#[test]
fn the_tip_lists_are_read_at_the_base_ref() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &contents(".assay/owners", "release%2F1"),
            json(&file_answer(".assay/owners", OWNERS)),
        )
        .ok(
            Method::Get,
            &contents(".assay/agents", "release%2F1"),
            json(&file_answer(".assay/agents", AGENTS)),
        );
    let waits = Waits::default();

    let lists = github(&recorded, &waits).lists_at("release/1");

    assert_eq!(
        lists.expect("read"),
        TipListFiles {
            owners: Some(OWNERS.to_vec()),
            agents: Some(AGENTS.to_vec()),
        }
    );
    assert_eq!(recorded.unused(), 0);
}

#[test]
fn an_absent_tip_list_is_absent() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &contents(".assay/owners", "main"),
            json(&file_answer(".assay/owners", OWNERS)),
        )
        .then(Method::Get, &contents(".assay/agents", "main"), absent());
    let waits = Waits::default();

    let lists = github(&recorded, &waits).lists_at("main");

    assert_eq!(
        lists.expect("read"),
        TipListFiles {
            owners: Some(OWNERS.to_vec()),
            agents: None,
        }
    );
}

#[test]
fn a_tip_list_at_the_cap_is_read_and_one_byte_over_is_refused_undecoded() {
    let at_cap = vec![b'#'; MAX_TIP_LIST_BYTES];
    let mut over = file_answer(".assay/agents", b"");
    over["size"] = Value::from(MAX_TIP_LIST_BYTES + 1);
    over["content"] = Value::from("not base64, never decoded");
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &contents(".assay/owners", "main"),
            json(&file_answer(".assay/owners", &at_cap)),
        )
        .ok(Method::Get, &contents(".assay/agents", "main"), json(&over));
    let waits = Waits::default();

    let lists = github(&recorded, &waits).lists_at("main");

    let size_over = u64::try_from(MAX_TIP_LIST_BYTES + 1).expect("fits");
    assert!(
        matches!(
            lists,
            Err(TipListsError::TooLarge { path: ".assay/agents", size }) if size == size_over
        ),
        "{lists:?}"
    );
    assert_eq!(recorded.unused(), 0, "the file at the cap was read first");
}

#[test]
fn a_tip_list_that_is_a_directory_is_refused() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &contents(".assay/owners", "main"),
        fixture("contents_workflows.json"),
    );
    let waits = Waits::default();

    let lists = github(&recorded, &waits).lists_at("main");

    assert!(
        matches!(
            lists,
            Err(TipListsError::NotAFile {
                path: ".assay/owners",
                entry: TipEntry::Directory
            })
        ),
        "{lists:?}"
    );
    assert_eq!(recorded.unused(), 0, "nothing more is read");
}

#[test]
fn a_tip_list_that_is_a_symlink_is_refused() {
    let entry = json!({
        "type": "symlink",
        "target": "../elsewhere/owners",
        "size": 19,
        "name": "owners",
        "path": ".assay/owners",
        "sha": "2d9debffacb6a0e831b00abdd15ce96ac50e0cc3",
    });
    // GitHub follows a symlink to a file of the repository and answers with that file.
    let followed = file_answer("docs/owners", OWNERS);
    for answer in [entry, followed] {
        let recorded = Recorded::default().ok(
            Method::Get,
            &contents(".assay/owners", "main"),
            json(&answer),
        );
        let waits = Waits::default();

        let lists = github(&recorded, &waits).lists_at("main");

        assert!(
            matches!(
                lists,
                Err(TipListsError::NotAFile {
                    path: ".assay/owners",
                    entry: TipEntry::Symlink
                })
            ),
            "{lists:?}"
        );
    }
}

/// The open PR listing.
fn open_pulls() -> String {
    path("pulls?state=open&per_page=100")
}

#[test]
fn the_poll_lists_open_pulls_with_their_head_and_base() {
    let recorded =
        Recorded::default().ok(Method::Get, &open_pulls(), fixture("open_pulls_all.json"));
    let waits = Waits::default();

    let pulls = PollForge::open_pulls(&github(&recorded, &waits), MAX_POLL_PULLS);

    let expected: Vec<PolledPull> = [
        (277, "bd0edf7171ec0528e6f29f7ea0e59025a401e9f2"),
        (271, "347cb035794294f93a668216d86258ef4ad13979"),
        (270, "00cacbbedc7aca477c6a470abca4e5e394174db6"),
    ]
    .into_iter()
    .map(|(number, head)| PolledPull {
        number: id(number),
        head_sha: head.parse().expect("a full commit id"),
        base_ref: "main".to_owned(),
    })
    .collect();
    assert_eq!(pulls.expect("listed"), expected);
}

#[test]
fn the_poll_reads_no_more_open_pulls_than_its_limit() {
    let recorded_pulls = fixture_json("open_pulls_all.json");
    let template = recorded_pulls.get(0).cloned().expect("a recorded open PR");
    let listing: Vec<Value> = (1..=60_u64)
        .map(|number| {
            let mut pull = template.clone();
            pull["number"] = Value::from(number);
            pull
        })
        .collect();
    let recorded = Recorded::default().ok(Method::Get, &open_pulls(), json(&Value::Array(listing)));
    let waits = Waits::default();

    let pulls = PollForge::open_pulls(&github(&recorded, &waits), MAX_POLL_PULLS);

    let numbers: Vec<u64> = pulls
        .expect("listed")
        .iter()
        .map(|pull| pull.number.get())
        .collect();
    assert_eq!(numbers, (1..=50).collect::<Vec<_>>());
    assert_eq!(recorded.unused(), 0, "one page");
}

/// The verdict checks on [`HEAD`](super::recorded::HEAD).
fn verdict_checks() -> String {
    path(&format!(
        "commits/{}/check-runs?filter=all&per_page=100&check_name=assay%20%2F%20verdict",
        super::recorded::HEAD
    ))
}

/// The verdict check's name on the default branch.
fn verdict() -> CheckName {
    CheckName::for_base("main", "main")
}

/// `check_runs_same_name.json` (another app's completed check, then the App's in progress) with
/// the App's check completed by `edit`.
fn completed_app_check(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut listing = fixture_json("check_runs_same_name.json");
    let app = &mut listing["check_runs"][1];
    app["status"] = Value::from("completed");
    app["conclusion"] = Value::from("failure");
    app["output"]["title"] = Value::from("assay: failure (trust-path-unapproved)");
    app["completed_at"] = Value::from("2026-10-18T21:50:07Z");
    edit(app);
    json(&listing)
}

#[test]
fn the_verdict_check_reads_its_conclusion_title_and_completion() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &verdict_checks(),
        completed_app_check(|_app| {}),
    );
    let waits = Waits::default();

    let view = github(&recorded, &waits).verdict_check(&head(), &verdict());

    assert_eq!(
        view.expect("listed"),
        Some(VerdictView::Completed(CompletedVerdict {
            conclusion: ObservedConclusion::Failure,
            title: Some("assay: failure (trust-path-unapproved)".to_owned()),
            completed_at: "2026-10-18T21:50:07Z".parse().expect("a timestamp"),
        }))
    );
}

#[test]
fn only_the_apps_check_is_the_verdict() {
    let waits = Waits::default();
    let foreign_only = Recorded::default().ok(
        Method::Get,
        &verdict_checks(),
        fixture("check_runs_foreign_only.json"),
    );
    let beside_foreign = Recorded::default().ok(
        Method::Get,
        &verdict_checks(),
        fixture("check_runs_same_name.json"),
    );

    let none = github(&foreign_only, &waits).verdict_check(&head(), &verdict());
    let pending = github(&beside_foreign, &waits).verdict_check(&head(), &verdict());

    assert_eq!(none.expect("listed"), None);
    assert_eq!(pending.expect("listed"), Some(VerdictView::Pending));
}

#[test]
fn two_app_verdict_checks_are_ambiguous() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &verdict_checks(),
        fixture("check_runs_two_app.json"),
    );
    let waits = Waits::default();

    let view = github(&recorded, &waits).verdict_check(&head(), &verdict());

    assert!(
        matches!(
            view,
            Err(ForgeError::Ambiguous {
                operation: Operation::ListChecks
            })
        ),
        "{view:?}"
    );
}

#[test]
fn a_completed_verdict_check_without_its_completion_time_is_refused() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &verdict_checks(),
        completed_app_check(|app| app["completed_at"] = Value::Null),
    );
    let waits = Waits::default();

    let view = github(&recorded, &waits).verdict_check(&head(), &verdict());

    assert!(
        matches!(
            view,
            Err(ForgeError::Malformed {
                operation: Operation::ListChecks,
                detail: "a completed check has no completed_at"
            })
        ),
        "{view:?}"
    );
}

#[test]
fn the_poll_reads_the_reviews() {
    let reviews = path("pulls/268/reviews?per_page=100");
    let polled = Recorded::default().ok(Method::Get, &reviews, fixture("reviews.json"));
    let forged = Recorded::default().ok(Method::Get, &reviews, fixture("reviews.json"));
    let waits = Waits::default();

    let read = PollForge::reviews(&github(&polled, &waits), pr());

    let expected = Forge::reviews(&github(&forged, &waits), pr()).expect("listed");
    assert_eq!(read.expect("listed"), expected);
    assert!(!expected.is_empty(), "the recording has reviews");
}

#[test]
fn a_tip_list_whose_content_disagrees_with_its_size_is_refused() {
    let mut long = file_answer(".assay/owners", OWNERS);
    long["size"] = Value::from(OWNERS.len() + 1);
    let recorded =
        Recorded::default().ok(Method::Get, &contents(".assay/owners", "main"), json(&long));
    let waits = Waits::default();

    let lists = github(&recorded, &waits).lists_at("main");

    assert!(
        matches!(
            lists,
            Err(TipListsError::Forge(ForgeError::Malformed {
                operation: Operation::ReadContents,
                detail: "the content does not match its size"
            }))
        ),
        "{lists:?}"
    );
}
