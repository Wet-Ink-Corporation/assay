//! `assay guard` end to end (#60 ST-2): the binary with a cleared environment holding only the
//! GitHub Actions variables (and, for a dispatch, the run token and a stand-in `gh` on `PATH`
//! that answers from a file and records its calls; no network), its `GITHUB_OUTPUT` bytes and
//! its exit codes (03 §2: 0 written, 1 refused, 2 cannot run).

use std::error::Error;
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

    /// Forge's recorded payload `name` as the event payload.
    fn recorded(&self, name: &str) -> Result<&Self, Box<dyn Error>> {
        let recorded = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../assay-forge/tests/fixtures/github/events")
            .join(name);
        fs::copy(recorded, self.path("event.json"))?;
        Ok(self)
    }

    /// A stand-in `gh` that answers `GET repos/<REPO>/pulls/2` with `pull` and anything else
    /// with a 404, recording each call's arguments in `bin/calls`.
    fn gh_answering(&self, pull: &Value) -> Result<(), Box<dyn Error>> {
        let bin = self.path("bin");
        fs::write(bin.join("pull"), format!("HTTP/2.0 200 OK\n\r\n{pull}"))?;
        let script = format!(
            "#!/bin/sh\ndir=${{0%/*}}\n\
             {{ printf call; for arg; do printf ' %s' \"$arg\"; done; printf '\\n'; }} >> \"$dir/calls\"\n\
             for last; do :; done\ncase \"$last\" in\n\
             'repos/{REPO}/pulls/2') /bin/cat \"$dir/pull\" ;;\n\
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

    /// `assay guard` for `event`, with only the Actions variables and `extra`.
    fn guard(&self, event: &str, extra: &[(&str, &str)]) -> Result<Ran, Box<dyn Error>> {
        let output = Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .arg("guard")
            .env("GITHUB_RUN_ID", "4242")
            .env("GITHUB_RUN_ATTEMPT", "1")
            .env("GITHUB_REPOSITORY_ID", REPO_ID)
            .env("GITHUB_REPOSITORY", REPO)
            .env("GITHUB_EVENT_NAME", event)
            .env("GITHUB_EVENT_PATH", self.path("event.json"))
            .env("GITHUB_OUTPUT", self.path("output"))
            .envs(extra.iter().copied())
            .output()?;
        Ok(Ran {
            code: output.status.code(),
            stderr: String::from_utf8(output.stderr)?,
            output: fs::read_to_string(self.path("output"))?,
        })
    }

    /// `assay guard` for a dispatch, with the run token and `gh` first on `PATH`.
    fn dispatch(&self) -> Result<Ran, Box<dyn Error>> {
        let bin = self.path("bin");
        let path = format!("{}:/usr/bin:/bin", bin.display());
        self.guard(
            "workflow_dispatch",
            &[("ASSAY_RUN_TOKEN", "ghs_run"), ("PATH", &path)],
        )
    }
}

/// PR 2 as `GET pulls/2` answers it: open, a draft, its head in this repository.
fn pull() -> Value {
    json!({
        "number": 2, "state": "open", "draft": true,
        "user": {"id": 21_031_067, "type": "User"},
        "head": {"sha": HEAD, "repo": {"id": 1_296_269}},
        "base": {"sha": BASE, "ref": "main"},
    })
}

#[test]
fn a_same_repository_opened_pr_writes_run_and_its_identity() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let ran = scene
        .recorded("event_opened.json")?
        .guard("pull_request_target", &[])?;

    assert_eq!((ran.code, ran.stderr.as_str()), (Some(0), ""), "{ran:?}");
    assert_eq!(
        ran.output,
        format!(
            "run=true\npr=2\nhead_sha={HEAD}\nbase_sha={BASE}\nbase_ref=main\ndraft=false\n\
             author_id=21031067\nsender_id=583231\ntrigger=opened\ndefault_branch=main\n"
        )
    );
    Ok(())
}

#[test]
fn a_fork_head_exits_1_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let ran = scene
        .recorded("event_opened_fork.json")?
        .guard("pull_request_target", &[])?;

    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(ran.stderr.contains("untrusted head repository"), "{ran:?}");
    assert_eq!(ran.output, "");
    Ok(())
}

#[test]
fn an_edit_without_a_base_change_writes_only_run_false() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let ran = scene
        .recorded("event_edited_title.json")?
        .guard("pull_request_target", &[])?;

    assert_eq!(
        (ran.code, ran.output.as_str()),
        (Some(0), "run=false\n"),
        "{ran:?}"
    );
    Ok(())
}

#[test]
fn an_event_it_does_not_run_for_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene.recorded("event_opened.json")?;

    let unknown = scene.guard("pull_request", &[])?;
    scene.recorded("event_labeled.json")?;
    let labeled = scene.guard("pull_request_target", &[])?;

    for (ran, named) in [(unknown, "GITHUB_EVENT_NAME"), (labeled, "\"labeled\"")] {
        assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
        assert!(ran.stderr.contains(named), "{ran:?}");
    }
    Ok(())
}

#[test]
fn a_payload_that_cannot_be_read_whole_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let missing = scene.guard("pull_request_target", &[])?;
    fs::write(scene.path("event.json"), vec![b' '; 8 * 1024 * 1024 + 1])?;
    let over_cap = scene.guard("pull_request_target", &[])?;

    for (ran, named) in [(missing, "GITHUB_EVENT_PATH"), (over_cap, "cap")] {
        assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
        assert!(ran.stderr.contains(named), "{ran:?}");
    }
    Ok(())
}

#[test]
fn a_payload_for_another_repository_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let ran = scene
        .recorded("event_opened.json")?
        .guard("pull_request_target", &[("GITHUB_REPOSITORY_ID", "7")])?;

    assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
    assert!(ran.stderr.contains("GITHUB_REPOSITORY_ID"), "{ran:?}");
    Ok(())
}

#[test]
fn a_dispatch_reads_the_pr_through_gh_and_writes_its_identity() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene
        .recorded("event_dispatch.json")?
        .gh_answering(&pull())?;

    let ran = scene.dispatch()?;

    assert_eq!((ran.code, ran.stderr.as_str()), (Some(0), ""), "{ran:?}");
    assert_eq!(
        ran.output,
        format!(
            "run=true\npr=2\nhead_sha={HEAD}\nbase_sha={BASE}\nbase_ref=main\ndraft=true\n\
             author_id=21031067\nsender_id=583231\ntrigger=workflow_dispatch\n\
             default_branch=main\n"
        )
    );
    assert_eq!(
        scene.calls()?,
        format!(
            "call api --include --method GET --header Accept: application/vnd.github+json \
             --header X-GitHub-Api-Version: 2022-11-28 -- repos/{REPO}/pulls/2\n"
        )
    );
    Ok(())
}

#[test]
fn a_dispatch_for_a_closed_pr_or_a_fork_head_exits_1() -> Result<(), Box<dyn Error>> {
    let mut closed = pull();
    closed["state"] = json!("closed");
    let mut fork = pull();
    fork["head"]["repo"]["id"] = json!(186_853_002);
    for (pull, refusal) in [
        (closed, "PR #2 is closed"),
        (fork, "untrusted head repository"),
    ] {
        let scene = Scene::new()?;
        scene.recorded("event_dispatch.json")?.gh_answering(&pull)?;

        let ran = scene.dispatch()?;

        assert_eq!((ran.code, ran.output.as_str()), (Some(1), ""), "{ran:?}");
        assert!(ran.stderr.contains(refusal), "{ran:?}");
    }
    Ok(())
}

#[test]
fn a_dispatch_without_the_run_token_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene
        .recorded("event_dispatch.json")?
        .gh_answering(&pull())?;
    let path = format!("{}:/usr/bin:/bin", scene.path("bin").display());

    let ran = scene.guard("workflow_dispatch", &[("PATH", &path)])?;

    assert_eq!((ran.code, ran.output.as_str()), (Some(2), ""), "{ran:?}");
    assert!(ran.stderr.contains("ASSAY_RUN_TOKEN"), "{ran:?}");
    assert_eq!(scene.calls()?, "");
    Ok(())
}

#[test]
fn guard_takes_no_arguments() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    scene.recorded("event_opened.json")?;

    let output = Command::new(env!("CARGO_BIN_EXE_assay"))
        .env_clear()
        .args(["guard", "--json"])
        .env("GITHUB_OUTPUT", scene.path("output"))
        .output()?;

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "assay: guard takes no arguments\n"
    );
    assert_eq!(fs::read_to_string(scene.path("output"))?, "");
    Ok(())
}
