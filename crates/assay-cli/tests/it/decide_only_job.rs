//! `assay decide-only guard` end to end (#61 DO-10): the binary with a cleared environment
//! holding only the GitHub Actions variables, the `assay-verdict` App's ids (variables, not
//! secrets) and, for a read, the run token and a stand-in `gh` on `PATH` that answers from files
//! and records its calls and the token it was given (no network); its `GITHUB_OUTPUT` bytes and
//! its exit codes (0 written, 1 refused, 2 cannot run). No App, ledger or OAuth token is ever
//! in the environment.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::write_executable;

/// The fixtures' repository, `Codertocat/Hello-World`.
const REPO: &str = "Codertocat/Hello-World";
const REPO_ID: &str = "1296269";
const HEAD: &str = "ec26c3e57ca3a959ca5aad62de7213c562f8c821";
const BASE: &str = "f95f852bd8fca8fcc58a9a2d6c842781e32a215e";
const FORK_ID: u64 = 186_853_002;
/// The `assay-verdict` App in these tests.
const APP_ID: u64 = 4_242;
const APP_BOT: u64 = 151_515;
/// The verdict check's name on a PR into the default branch, as the check-runs query has it.
const CHECK_QUERY: &str = "filter=all&per_page=100&check_name=assay%20%2F%20verdict";

/// How the binary ended.
#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    stderr: String,
    /// `GITHUB_OUTPUT`'s bytes afterwards.
    output: String,
}

/// A scratch directory with the payload, an empty `GITHUB_OUTPUT` and a `bin/` for `gh`.
struct Scene {
    root: TempDir,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("output"), "")?;
        fs::create_dir(root.path().join("bin"))?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Forge's recorded decide-only payload `name`, changed by `edit`, as the event payload.
    fn payload(&self, name: &str, edit: impl FnOnce(&mut Value)) -> Result<&Self, Box<dyn Error>> {
        let recorded = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../assay-forge/tests/fixtures/github/events/decide_only")
            .join(name);
        let mut payload: Value = serde_json::from_slice(&fs::read(recorded)?)?;
        edit(&mut payload);
        fs::write(self.path("event.json"), payload.to_string())?;
        Ok(self)
    }

    /// A stand-in `gh` that answers `GET repos/<REPO>/<path>` with each `(path, body)` and
    /// anything else with a 404, recording each call's last argument and its `GH_TOKEN` in
    /// `bin/calls`.
    fn gh_answering<P: AsRef<str>>(&self, answers: &[(P, Value)]) -> Result<(), Box<dyn Error>> {
        let bin = self.path("bin");
        let mut cases = String::new();
        for (index, (path, body)) in answers.iter().enumerate() {
            let path = path.as_ref();
            fs::write(
                bin.join(format!("answer{index}")),
                format!("HTTP/2.0 200 OK\n\r\n{body}"),
            )?;
            let target = if path.is_empty() {
                format!("repos/{REPO}")
            } else {
                format!("repos/{REPO}/{path}")
            };
            writeln!(cases, "'{target}') /bin/cat \"$dir/answer{index}\" ;;")?;
        }
        let script = format!(
            "#!/bin/sh\ndir=${{0%/*}}\nfor last; do :; done\n\
             printf '%s %s\\n' \"$GH_TOKEN\" \"$last\" >> \"$dir/calls\"\n\
             case \"$last\" in\n{cases}\
             *) printf 'HTTP/2.0 404 Not Found\\n\\r\\n{{\"message\":\"Not Found\"}}'; exit 1 ;;\n\
             esac\n"
        );
        write_executable(&bin.join("gh"), &script)?;
        Ok(())
    }

    /// The calls the stand-in `gh` recorded.
    fn calls(&self) -> Result<String, Box<dyn Error>> {
        let calls = self.path("bin").join("calls");
        Ok(if calls.exists() {
            fs::read_to_string(calls)?
        } else {
            String::new()
        })
    }

    /// `assay decide-only guard` for `event`, with only the Actions variables, the App's ids
    /// and `extra`.
    fn guard(&self, event: &str, extra: &[(&str, &str)]) -> Result<Ran, Box<dyn Error>> {
        let output = Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .args(["decide-only", "guard"])
            .env("GITHUB_RUN_ID", "4242")
            .env("GITHUB_RUN_ATTEMPT", "1")
            .env("GITHUB_REPOSITORY_ID", REPO_ID)
            .env("GITHUB_REPOSITORY", REPO)
            .env("GITHUB_EVENT_NAME", event)
            .env("GITHUB_EVENT_PATH", self.path("event.json"))
            .env("GITHUB_OUTPUT", self.path("output"))
            .env("ASSAY_VERDICT_APP_ID", APP_ID.to_string())
            .env("ASSAY_VERDICT_BOT_ID", APP_BOT.to_string())
            .envs(extra.iter().copied())
            .output()?;
        Ok(Ran {
            code: output.status.code(),
            stderr: String::from_utf8(output.stderr)?,
            output: fs::read_to_string(self.path("output"))?,
        })
    }

    /// `assay decide-only guard` for `event` with the run token and `gh` first on `PATH`.
    fn guard_reading(&self, event: &str) -> Result<Ran, Box<dyn Error>> {
        let path = format!("{}:/usr/bin:/bin", self.path("bin").display());
        self.guard(event, &[("ASSAY_RUN_TOKEN", "ghs_run"), ("PATH", &path)])
    }
}

/// PR `number` as `GET pulls/<n>` answers it: open, its head `head` in this repository.
fn pull(number: u64, head: &str) -> Value {
    json!({
        "number": number, "state": "open", "draft": false,
        "user": {"id": 21_031_067, "type": "User"},
        "head": {"sha": head, "repo": {"id": 1_296_269}},
        "base": {"sha": BASE, "ref": "main"},
    })
}

#[test]
fn a_users_recheck_on_a_pr_writes_that_pr_and_trigger_comment() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene
        .payload("comment_created_pr.json", |_| {})?
        .gh_answering(&[("pulls/2", pull(2, HEAD))])?;

    let ran = scene.guard_reading("issue_comment")?;

    assert_eq!((ran.code, ran.stderr.as_str()), (Some(0), ""), "{ran:?}");
    assert_eq!(ran.output, "prs=[2]\ntrigger=comment\n");
    assert_eq!(scene.calls()?, format!("ghs_run repos/{REPO}/pulls/2\n"));
    Ok(())
}

#[test]
fn an_event_that_is_no_reason_to_decide_writes_prs_empty_with_no_token()
-> Result<(), Box<dyn Error>> {
    type Edit = fn(&mut Value);
    let cases: [(&str, &str, Edit); 7] = [
        ("issue_comment", "comment_created_via_app.json", |_| {}),
        ("issue_comment", "comment_created_issue.json", |_| {}),
        ("issue_comment", "comment_edited.json", |_| {}),
        ("issue_comment", "comment_created_pr.json", |v| {
            v["comment"]["body"] = json!("LGTM, /assay recheck later");
        }),
        ("pull_request_target", "label_labeled_user.json", |v| {
            v["label"]["name"] = json!("bug");
        }),
        ("pull_request_target", "label_labeled_user.json", |v| {
            v["sender"]["id"] = json!(APP_BOT);
            v["sender"]["type"] = json!("Bot");
        }),
        ("pull_request_target", "label_unlabeled_bot.json", |_| {}),
    ];
    for (event, name, edit) in cases {
        let scene = Scene::new()?;
        scene.payload(name, edit)?;

        // No token and no PATH: a read would fail, so none is made.
        let ran = scene.guard(event, &[])?;

        assert_eq!(
            (ran.code, ran.stderr.as_str(), ran.output.as_str()),
            (Some(0), "", "prs=[]\n"),
            "{name}: {ran:?}"
        );
    }
    Ok(())
}

#[test]
fn a_users_assay_label_writes_its_pr_and_trigger_label_with_no_token() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;
    scene.payload("label_labeled_user.json", |_| {})?;

    let ran = scene.guard("pull_request_target", &[])?;

    assert_eq!((ran.code, ran.stderr.as_str()), (Some(0), ""), "{ran:?}");
    assert_eq!(ran.output, "prs=[2]\ntrigger=label\n");
    Ok(())
}

#[test]
fn a_command_on_a_fork_or_deleted_head_exits_1_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    for (repo, refusal) in [
        (
            json!({"id": FORK_ID}),
            "PR #2's head is in repository 186853002",
        ),
        (Value::Null, "PR #2's head repository was deleted"),
    ] {
        let mut answer = pull(2, HEAD);
        answer["head"]["repo"] = repo;
        let scene = Scene::new()?;
        scene
            .payload("comment_created_pr.json", |_| {})?
            .gh_answering(&[("pulls/2", answer)])?;

        let ran = scene.guard_reading("issue_comment")?;

        assert_eq!((ran.code, ran.output.as_str()), (Some(1), ""), "{ran:?}");
        assert!(ran.stderr.contains(refusal), "{ran:?}");
        assert!(ran.stderr.contains("(0006)"), "{ran:?}");
    }
    Ok(())
}

#[test]
fn a_label_on_a_fork_or_deleted_head_exits_1_from_the_payload() -> Result<(), Box<dyn Error>> {
    for repo in [json!({"id": FORK_ID}), Value::Null] {
        let scene = Scene::new()?;
        scene.payload("label_labeled_user.json", |v| {
            v["pull_request"]["head"]["repo"] = repo;
        })?;

        let ran = scene.guard("pull_request_target", &[])?;

        assert_eq!((ran.code, ran.output.as_str()), (Some(1), ""), "{ran:?}");
        assert!(ran.stderr.contains("untrusted head repository"), "{ran:?}");
    }
    Ok(())
}

#[test]
fn a_payload_for_another_repository_cannot_run() -> Result<(), Box<dyn Error>> {
    for (event, name) in [
        ("issue_comment", "comment_created_pr.json"),
        ("pull_request_target", "label_labeled_user.json"),
        ("schedule", "schedule.json"),
    ] {
        let scene = Scene::new()?;
        scene.payload(name, |_| {})?;

        let ran = scene.guard(event, &[("GITHUB_REPOSITORY_ID", "7")])?;

        assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
        assert!(ran.stderr.contains("GITHUB_REPOSITORY_ID"), "{ran:?}");
        assert_eq!(scene.calls()?, "");
    }
    Ok(())
}

/// The App's verdict check on `head`, `failure` titled `title`, completed at `completed_at`.
fn checks(head: &str, title: &str, completed_at: &str) -> Value {
    json!({"check_runs": [{
        "id": 7, "name": "assay / verdict", "head_sha": head, "external_id": "x",
        "status": "completed", "conclusion": "failure", "completed_at": completed_at,
        "output": {"title": title, "text": null}, "app": {"id": APP_ID}, "html_url": null,
    }]})
}

/// An `APPROVED` review of `head` submitted at `at`.
fn approval(head: &str, at: &str) -> Value {
    json!([{
        "id": 9, "user": {"id": 583_231, "type": "User"}, "state": "APPROVED",
        "commit_id": head, "submitted_at": at,
    }])
}

/// The poll's answers: two open PRs, 2 waiting on an approval that arrived, 3 failing for
/// another reason.
fn poll_answers(pr_2_title: &str) -> Vec<(String, Value)> {
    let head_3 = "3".repeat(40);
    vec![
        (String::new(), json!({"default_branch": "main"})),
        (
            "pulls?state=open&per_page=100".to_owned(),
            json!([
                {"number": 3, "state": "open", "base": {"ref": "main"},
                 "head": {"sha": head_3, "ref": "c", "repo": {"full_name": REPO}}},
                {"number": 2, "state": "open", "base": {"ref": "main"},
                 "head": {"sha": HEAD, "ref": "b", "repo": {"full_name": REPO}}},
            ]),
        ),
        (
            format!("commits/{HEAD}/check-runs?{CHECK_QUERY}"),
            checks(HEAD, pr_2_title, "2026-10-31T10:00:00Z"),
        ),
        (
            format!("commits/{head_3}/check-runs?{CHECK_QUERY}"),
            checks(
                &head_3,
                "assay: failure (gate-error)",
                "2026-10-31T09:00:00Z",
            ),
        ),
        (
            "pulls/2/reviews?per_page=100".to_owned(),
            approval(HEAD, "2026-10-31T10:05:00Z"),
        ),
        ("pulls/2".to_owned(), pull(2, HEAD)),
    ]
}

#[test]
fn the_poll_writes_the_prs_approved_since_their_check_and_trigger_schedule()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene
        .payload("schedule.json", |_| {})?
        .gh_answering(&poll_answers("assay: failure (trust-path-unapproved)"))?;

    let ran = scene.guard_reading("schedule")?;

    assert_eq!((ran.code, ran.stderr.as_str()), (Some(0), ""), "{ran:?}");
    assert_eq!(ran.output, "prs=[2]\ntrigger=schedule\n");
    let head_3 = "3".repeat(40);
    assert_eq!(
        scene.calls()?,
        format!(
            "ghs_run repos/{REPO}\n\
             ghs_run repos/{REPO}/pulls?state=open&per_page=100\n\
             ghs_run repos/{REPO}/commits/{head_3}/check-runs?{CHECK_QUERY}\n\
             ghs_run repos/{REPO}/commits/{HEAD}/check-runs?{CHECK_QUERY}\n\
             ghs_run repos/{REPO}/pulls/2/reviews?per_page=100\n\
             ghs_run repos/{REPO}/pulls/2\n"
        )
    );
    Ok(())
}

#[test]
fn a_poll_with_nothing_waiting_writes_prs_empty() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene
        .payload("schedule.json", |_| {})?
        .gh_answering(&poll_answers("assay: failure (required-role-degraded)"))?;

    let ran = scene.guard_reading("schedule")?;

    assert_eq!(
        (ran.code, ran.stderr.as_str(), ran.output.as_str()),
        (Some(0), "", "prs=[]\n"),
        "{ran:?}"
    );
    Ok(())
}

#[test]
fn a_read_that_fails_cannot_run_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    // The stand-in answers nothing, so the PR read is a 404.
    scene
        .payload("comment_created_pr.json", |_| {})?
        .gh_answering::<&str>(&[])?;

    let ran = scene.guard_reading("issue_comment")?;

    assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
    assert!(ran.stderr.contains("reading PR #2 failed"), "{ran:?}");
    Ok(())
}

#[test]
fn a_command_without_the_run_token_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene
        .payload("comment_created_pr.json", |_| {})?
        .gh_answering(&[("pulls/2", pull(2, HEAD))])?;
    let path = format!("{}:/usr/bin:/bin", scene.path("bin").display());

    let ran = scene.guard("issue_comment", &[("PATH", &path)])?;

    assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
    assert!(ran.stderr.contains("ASSAY_RUN_TOKEN"), "{ran:?}");
    assert_eq!(scene.calls()?, "");
    Ok(())
}

#[test]
fn the_app_s_ids_are_required() -> Result<(), Box<dyn Error>> {
    for (name, value) in [("ASSAY_VERDICT_APP_ID", "0"), ("ASSAY_VERDICT_BOT_ID", "x")] {
        let scene = Scene::new()?;
        scene.payload("label_labeled_user.json", |_| {})?;

        let ran = scene.guard("pull_request_target", &[(name, value)])?;

        assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
        assert!(ran.stderr.contains(name), "{ran:?}");
        assert!(!ran.stderr.contains(&format!("{value:?}")), "{ran:?}");
    }
    Ok(())
}

#[test]
fn an_event_decide_only_does_not_run_for_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene.payload("comment_created_pr.json", |_| {})?;

    let ran = scene.guard("workflow_dispatch", &[])?;

    assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
    assert!(ran.stderr.contains("GITHUB_EVENT_NAME"), "{ran:?}");
    Ok(())
}

#[test]
fn decide_only_takes_guard_alone_and_refuses_the_steps_not_built_yet() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;
    for (args, refusal) in [
        (&["decide-only"][..], "usage: assay decide-only guard"),
        (
            &["decide-only", "verdict"][..],
            "usage: assay decide-only guard",
        ),
        (
            &["decide-only", "guard", "--json"][..],
            "usage: assay decide-only guard",
        ),
        (
            &["decide-only", "decide"][..],
            "usage: assay decide-only decide --config <plugins-toml> --out <dir>",
        ),
        (&["decide-only", "publish"][..], "not built yet"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .args(args)
            .env("GITHUB_OUTPUT", scene.path("output"))
            .output()?;

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains(refusal), "{args:?}: {stderr}");
        assert_eq!(fs::read_to_string(scene.path("output"))?, "");
    }
    Ok(())
}
