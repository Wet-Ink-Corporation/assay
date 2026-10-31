//! `assay open --config <plugins-toml>` (#54, #60 ST-4) end to end: the binary with a cleared
//! environment, GitHub and the ledger repository answered by a stand-in `gh` that keeps their
//! state in files and records every call (no network), and the lock's deletion pushed through
//! a stand-in `git` that reports it and runs the real git for everything else.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assay_app::UserId;
use assay_cli::outputs::{Identity, Trigger};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

use crate::support::write_executable;

const REPO: &str = "Wet-Ink-Corporation/assay";
const REPO_ID: &str = "840213377";
const LEDGER: &str = "Wet-Ink-Corporation/assay-ledger";
const RUN_ID: &str = "39168909542";
const APP_ID: &str = "424242";
const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const APP_TOKEN: &str = "ghs_app_secret_value";
const LEDGER_TOKEN: &str = "ghs_ledger_secret_value";

/// The stand-in `gh`: GitHub's answers for the calls `open` makes, over state in `state.json`
/// beside it, each call appended to `calls`.
const GH: &str = r#"
import base64, hashlib, json, os, re, sys

here = os.path.dirname(os.path.abspath(sys.argv[0]))
args = sys.argv[1:]
method = args[args.index("--method") + 1]
path = args[-1]
body = json.loads(sys.stdin.read() or "null") if "--input" in args else None
with open(os.path.join(here, "calls"), "a") as calls:
    calls.write(method + " " + path + "\n")
state_file = os.path.join(here, "state.json")
state = json.load(open(state_file))
repo, ledger, run_id = state["repo"], state["ledger"], state["run_id"]

def answer(status, value):
    reason = {200: "OK", 201: "Created", 404: "Not Found", 422: "Unprocessable Entity"}[status]
    json.dump(state, open(state_file, "w"))
    sys.stdout.write("HTTP/2.0 %d %s\n\r\n%s" % (status, reason, json.dumps(value)))
    sys.exit(0 if status < 400 else 1)

def check(run):
    return dict(run, app={"id": state["app_id"]}, html_url="https://github.com/%s/runs/%d" % (repo, run["id"]))

def commit_view(sha):
    return {"sha": sha, "message": state["commits"][sha], "tree": {"sha": "4b825dc642cb6eb9a060e54bf8d69288fbee4904"}, "parents": []}

missing = {"message": "Not Found"}
if method == "GET" and path == "repos/%s/actions/runs/%s" % (repo, run_id):
    answer(200, {"id": int(run_id), "created_at": state["created_at"]})
if method == "GET" and path == "repos/" + ledger:
    answer(200, {"full_name": ledger})
m = re.fullmatch(r"repos/%s/contents/(.+)" % re.escape(ledger), path)
if m and method == "GET":
    name = m.group(1).replace("%2F", "/")
    if name not in state["files"]:
        answer(404, missing)
    raw = state["files"][name].encode()
    sha = hashlib.sha1(raw).hexdigest()
    answer(200, {"type": "file", "encoding": "base64", "size": len(raw), "path": name, "content": base64.b64encode(raw).decode(), "sha": sha})
if m and method == "PUT":
    state["files"][m.group(1).replace("%2F", "/")] = base64.b64decode(body["content"]).decode()
    answer(201, {})
if method == "POST" and path == "repos/%s/git/commits" % ledger:
    sha = hashlib.sha1(body["message"].encode()).hexdigest()
    state["commits"][sha] = body["message"]
    answer(201, commit_view(sha))
if method == "POST" and path == "repos/%s/git/refs" % ledger:
    if body["ref"] in state["refs"]:
        answer(422, {"message": "Reference already exists"})
    state["refs"][body["ref"]] = body["sha"]
    answer(201, {"ref": body["ref"], "object": {"sha": body["sha"], "type": "commit"}})
m = re.fullmatch(r"repos/%s/git/ref/(.+)" % re.escape(ledger), path)
if m and method == "GET":
    name = "refs/" + m.group(1)
    if name not in state["refs"]:
        answer(404, missing)
    answer(200, {"ref": name, "object": {"sha": state["refs"][name], "type": "commit"}})
m = re.fullmatch(r"repos/%s/git/commits/([0-9a-f]{40})" % re.escape(ledger), path)
if m and method == "GET":
    answer(200, commit_view(m.group(1)))
if method == "GET" and path.startswith("repos/%s/commits/" % repo) and "/check-runs" in path:
    head = path.split("/")[4]
    runs = [check(run) for run in state["checks"] if run["head_sha"] == head]
    answer(200, {"total_count": len(runs), "check_runs": runs})
if method == "POST" and path == "repos/%s/check-runs" % repo:
    run = {"id": 1000 + len(state["checks"]), "name": body["name"], "head_sha": body["head_sha"], "external_id": body.get("external_id"), "status": body.get("status", "queued"), "conclusion": None, "output": {"title": body["output"]["title"], "text": body["output"].get("text")}}
    state["checks"].append(run)
    answer(201, check(run))
answer(404, missing)
"#;

/// The stand-in `git`: a push reports its ref deleted, as `--porcelain` does, and records its
/// last argument; anything else is the real git.
fn git_stand_in(real: &Path) -> String {
    format!(
        "#!/bin/sh\nfor arg; do if [ \"$arg\" = push ]; then pushed=1; fi; done\n\
         if [ -n \"$pushed\" ]; then\n  for last; do :; done\n  \
         printf '%s\\n' \"$last\" >> \"${{0%/*}}/pushes\"\n  \
         printf 'To https://github.com/{LEDGER}.git\\n-\\t%s\\t[deleted]\\nDone\\n' \"$last\"\n  \
         exit 0\nfi\nexec '{}' \"$@\"\n",
        real.display()
    )
}

/// The first `name` on `PATH`.
fn on_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("no {name} on PATH").into())
}

/// A run of `open` in a scratch directory with base config, `gh` and `git` stand-ins.
struct Scene {
    root: TempDir,
}

impl Scene {
    /// Base config whose `[assay]` table ends with `extra`.
    fn new(extra: &str) -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin)?;
        fs::create_dir_all(root.path().join("base/.assay"))?;
        fs::create_dir_all(root.path().join("home"))?;
        fs::write(
            root.path().join("base/.assay/plugins.toml"),
            format!("[assay]\ndigest = \"{DIGEST}\"\n{extra}"),
        )?;
        let python = on_path("python3")?;
        write_executable(&bin.join("gh"), &format!("#!{} -I\n{GH}", python.display()))?;
        write_executable(&bin.join("git"), &git_stand_in(&on_path("git")?))?;
        let state = json!({
            "repo": REPO, "ledger": LEDGER, "run_id": RUN_ID, "app_id": 424_242,
            "created_at": "2026-10-31T09:00:00Z",
            "files": {}, "refs": {}, "commits": {}, "checks": [],
        });
        fs::write(bin.join("state.json"), state.to_string())?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// `assay open --config base/.assay/plugins.toml` with only `variables` and `PATH`.
    fn open(&self, variables: &[(&str, &str)]) -> Result<Output, Box<dyn Error>> {
        let mut path = vec![self.path("bin")];
        path.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        Ok(Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .current_dir(self.root.path())
            .args(["open", "--config", "base/.assay/plugins.toml"])
            .env("PATH", env::join_paths(path)?)
            .env("HOME", self.path("home"))
            .envs(variables.iter().copied())
            .output()?)
    }

    /// The calls the stand-in `gh` recorded, one per line.
    fn calls(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let calls = self.path("bin/calls");
        if !calls.exists() {
            return Ok(Vec::new());
        }
        Ok(fs::read_to_string(calls)?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// GitHub's and the ledger's state after the run.
    fn state(&self) -> Result<Value, Box<dyn Error>> {
        Ok(serde_json::from_str(&fs::read_to_string(
            self.path("bin/state.json"),
        )?)?)
    }
}

/// `guard`'s outputs for an `opened` PR 7 into `main`, as `toJSON(needs)` hands them over.
fn needs() -> Result<String, Box<dyn Error>> {
    let identity = Identity {
        pr: "7".parse()?,
        head_sha: HEAD.parse()?,
        base_sha: "a".repeat(40).parse()?,
        base_ref: "main".to_owned(),
        draft: false,
        author: UserId("11".parse()?),
        sender: UserId("12".parse()?),
        trigger: Trigger::Opened,
        default_branch: "main".to_owned(),
    };
    let outputs: Map<String, Value> = identity
        .outputs()
        .into_iter()
        .map(|(name, value)| (name.as_str().to_owned(), Value::String(value)))
        .collect();
    Ok(json!({"guard": {"result": "success", "outputs": outputs}}).to_string())
}

/// Every variable a first attempt needs, with `attempt` as `GITHUB_RUN_ATTEMPT`.
fn variables(attempt: &str, needs: &str) -> Vec<(&'static str, String)> {
    vec![
        ("GITHUB_RUN_ID", RUN_ID.to_owned()),
        ("GITHUB_RUN_ATTEMPT", attempt.to_owned()),
        ("GITHUB_REPOSITORY_ID", REPO_ID.to_owned()),
        ("GITHUB_REPOSITORY", REPO.to_owned()),
        ("ASSAY_NEEDS", needs.to_owned()),
        ("ASSAY_GITHUB_TOKEN", APP_TOKEN.to_owned()),
        ("ASSAY_LEDGER_TOKEN", LEDGER_TOKEN.to_owned()),
        ("ASSAY_VERDICT_APP_ID", APP_ID.to_owned()),
        ("ASSAY_VERDICT_BOT_ID", "424243".to_owned()),
    ]
}

/// `variables` as the borrowed pairs `Command::envs` takes, without `skip`.
fn pairs<'v>(variables: &'v [(&'static str, String)], skip: &str) -> Vec<(&'v str, &'v str)> {
    variables
        .iter()
        .filter(|(name, _)| *name != skip)
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

const LEDGER_CONFIG: &str = "ledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n";

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_binary_refuses_a_re_run_before_reading_a_token() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(LEDGER_CONFIG)?;
    let only_the_run = [
        ("GITHUB_RUN_ID", RUN_ID),
        ("GITHUB_RUN_ATTEMPT", "2"),
        ("GITHUB_REPOSITORY_ID", REPO_ID),
        ("GITHUB_REPOSITORY", REPO),
    ];

    let rerun = scene.open(&only_the_run)?;

    assert_eq!(
        (rerun.status.code(), rerun.stdout.is_empty()),
        (Some(2), true)
    );
    let refusal = String::from_utf8(rerun.stderr)?;
    assert!(refusal.contains("attempt 2"), "{refusal}");
    assert_eq!(scene.calls()?, Vec::<String>::new());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_first_attempt_opens_the_verdict_check_end_to_end() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(LEDGER_CONFIG)?;
    let variables = variables("1", &needs()?);

    let opened = scene.open(&pairs(&variables, ""))?;

    let stderr = String::from_utf8(opened.stderr)?;
    assert_eq!(
        (opened.status.code(), String::from_utf8(opened.stdout)?),
        (
            Some(0),
            format!(
                "assay open: assay / verdict check 1000 created in_progress, stamp \
                 pending:{RUN_ID}:1; opener record 1\n"
            )
        ),
        "{stderr}"
    );
    let state = scene.state()?;
    let record = state["files"][format!("pushes/{REPO_ID}/7.jsonl")]
        .as_str()
        .ok_or("no opener record")?;
    assert!(
        record.contains("\"at\":\"2026-10-31T09:00:00.000Z\""),
        "{record}"
    );
    assert_eq!(
        (
            state["checks"][0]["head_sha"].as_str(),
            state["checks"][0]["external_id"].as_str(),
            state["checks"][0]["status"].as_str(),
        ),
        (
            Some(HEAD),
            Some(format!("pending:{RUN_ID}:1").as_str()),
            Some("in_progress")
        )
    );
    let pushes = fs::read_to_string(scene.path("bin/pushes"))?;
    assert_eq!(
        pushes,
        format!(":refs/heads/lock-{REPO_ID}-{HEAD}\n"),
        "the lock was released by a push"
    );
    for secret in [APP_TOKEN, LEDGER_TOKEN] {
        assert!(!stderr.contains(secret), "{stderr}");
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_or_invalid_token_is_named_and_never_shown() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(LEDGER_CONFIG)?;
    let needs = needs()?;

    for name in ["ASSAY_GITHUB_TOKEN", "ASSAY_LEDGER_TOKEN"] {
        let variables = variables("1", &needs);
        let missing = scene.open(&pairs(&variables, name))?;
        let mut invalid = pairs(&variables, name);
        invalid.push((name, "not a token value"));
        let invalid = scene.open(&invalid)?;

        for (output, says) in [(missing, "is not set"), (invalid, "is not the")] {
            let stderr = String::from_utf8(output.stderr)?;
            assert_eq!(output.status.code(), Some(2), "{name}: {stderr}");
            assert!(
                stderr.contains(&format!("{name} {says}")),
                "{name}: {stderr}"
            );
            assert!(!stderr.contains("not a token value"), "{name}: {stderr}");
        }
    }
    assert_eq!(scene.calls()?, Vec::<String>::new());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_config_without_a_ledger_repo_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let variables = variables("1", &needs()?);

    let output = scene.open(&pairs(&variables, ""))?;

    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("[assay].ledger_repo"), "{stderr}");
    assert_eq!(scene.calls()?, Vec::<String>::new());
    Ok(())
}
