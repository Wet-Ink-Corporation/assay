//! `assay status [--json] [--pr <n>]` (#126, #273) end to end: the binary in a fixture checkout
//! whose `origin` is the assay repository, reading GitHub through a stand-in `gh` that answers
//! from files and records every call (no network), and the run bundle through its
//! `gh run download` (wi-6b26d6).

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::check::{Outcome, plain_git, write};
use crate::support::write_executable;

const REPO: &str = "Wet-Ink-Corporation/assay";
const REPO_ID: u64 = 840_213_377;
const APP_ID: &str = "424242";
const BOT_ID: &str = "424243";
const DECIDE: &str = "assay-decide-0123456789abcdef0123456789abcdef";
const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A checkout of the assay repository on `feature/status`, one commit past `origin/main`,
/// with a stand-in `gh` first on `PATH`.
struct Scene {
    root: TempDir,
    head: String,
    /// The API paths the stand-in answers, each with its response body.
    responses: Vec<(String, Value)>,
}

impl Scene {
    fn new(owners: &str) -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let work = root.path().join("work");
        plain_git(root.path(), &["init", "--quiet", "-b", "main", "work"])?;
        write(&work, ".assay/owners", owners)?;
        plain_git(&work, &["add", "--all"])?;
        plain_git(&work, &["commit", "--quiet", "-m", "base"])?;
        plain_git(&work, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
        let url = format!("https://github.com/{REPO}.git");
        plain_git(&work, &["remote", "add", "origin", &url])?;
        plain_git(&work, &["checkout", "--quiet", "-b", "feature/status"])?;
        write(&work, "src/lib.rs", "pub fn one() {}\n")?;
        plain_git(&work, &["add", "--all"])?;
        plain_git(&work, &["commit", "--quiet", "-m", "change"])?;
        let head = plain_git(&work, &["rev-parse", "HEAD"])?.trim().to_owned();
        fs::create_dir(root.path().join("home"))?;
        fs::create_dir(root.path().join("bin"))?;
        let pull = json!({
            "number": 42, "state": "open",
            "head": {"sha": head, "ref": "feature/status", "repo": {"full_name": REPO}},
            "base": {"ref": "main"},
        });
        let responses = vec![
            (
                format!("repos/{REPO}"),
                json!({"id": REPO_ID, "full_name": REPO, "default_branch": "main"}),
            ),
            // The same PR answers both reads; each answer owns its copy.
            (format!("repos/{REPO}/pulls/42"), pull.clone()),
            (
                format!(
                    "repos/{REPO}/pulls?state=open&head=Wet-Ink-Corporation:feature%2Fstatus\
                     &per_page=100"
                ),
                json!([pull]),
            ),
            (
                format!("repos/{REPO}/commits/{head}/check-runs?filter=all&per_page=100"),
                json!({"total_count": 0, "check_runs": []}),
            ),
            ("user".to_owned(), json!({"id": 3, "login": "someone"})),
        ];
        Ok(Self {
            root,
            head,
            responses,
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Answers `path` with `body`, replacing any answer it had.
    fn answer(&mut self, path: &str, body: Value) {
        self.responses.retain(|(known, _)| known != path);
        self.responses.push((path.to_owned(), body));
    }

    /// The verdict check of a decided run whose pointer names run 7 and `DIGEST`.
    fn with_decided_check(&mut self) {
        let key = format!("sha256:{}", "b".repeat(64));
        let inputs = format!("sha256:{}", "c".repeat(64));
        let text = format!(
            "```text\ndecision_key: {key}\ninputs_digest: {inputs}\nevidence pointer: run_id 7, \
             run_attempt 1, sha256 {DIGEST}\nevidence record: x\n```"
        );
        let check = json!({
            "id": 99, "name": "assay / verdict", "head_sha": self.head, "external_id": key,
            "status": "completed", "conclusion": "success",
            "output": {"title": "assay: success", "text": text},
            "app": {"id": 424_242},
            "html_url": format!("https://github.com/{REPO}/runs/99"),
        });
        self.answer(
            &format!(
                "repos/{REPO}/commits/{}/check-runs?filter=all&per_page=100",
                self.head
            ),
            json!({"total_count": 1, "check_runs": [check]}),
        );
        self.answer(
            &format!("repos/{REPO}/actions/runs/7/artifacts?per_page=100"),
            json!({"total_count": 1, "artifacts": [{
                "name": DECIDE, "size_in_bytes": 2, "expired": false,
                "workflow_run": {"id": 7, "repository_id": REPO_ID},
            }]}),
        );
    }

    /// Writes the stand-in `gh`: it appends its arguments to `calls`, one call per line;
    /// `gh run download` copies `bundle` into its `--dir`, and `gh api` prints the answer for
    /// its path, or a 404.
    fn stand_in(&self) -> Result<(), Box<dyn Error>> {
        let bin = self.path("bin");
        let mut cases = Vec::new();
        for (index, (path, body)) in self.responses.iter().enumerate() {
            let file = bin.join(format!("response-{index}"));
            fs::write(&file, format!("HTTP/2.0 200 OK\n\r\n{body}"))?;
            cases.push(format!("'{path}') /bin/cat '{}' ;;\n", file.display()));
        }
        let cases = cases.concat();
        fs::write(bin.join("bundle"), "{}")?;
        let script = format!(
            "#!/bin/sh\ndir=${{0%/*}}\n\
             {{ printf call; for arg; do printf ' %s' \"$arg\"; done; printf '\\n'; }} >> \"$dir/calls\"\n\
             if [ \"$1\" = run ]; then\n  while [ $# -gt 0 ]; do\n    \
             if [ \"$1\" = --dir ]; then dest=\"$2\"; fi\n    shift\n  done\n  \
             /bin/cp \"$dir/bundle\" \"$dest/run-inputs.json\"\n  exit 0\nfi\n\
             for last; do :; done\ncase \"$last\" in\n{cases}\
             *) printf 'HTTP/2.0 404 Not Found\\n\\r\\n{{\"message\":\"Not Found\"}}'; exit 1 ;;\n\
             esac\n"
        );
        write_executable(&bin.join("gh"), &script)?;
        Ok(())
    }

    /// The calls the stand-in recorded, one per line.
    fn calls(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let calls = self.path("bin").join("calls");
        if !calls.exists() {
            return Ok(Vec::new());
        }
        Ok(fs::read_to_string(calls)?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// `assay status <args>` in the checkout with `variables` and `gh` first on `PATH`.
    fn status(&self, args: &[&str], variables: &[(&str, &str)]) -> Result<Outcome, Box<dyn Error>> {
        self.stand_in()?;
        let mut path = vec![self.path("bin")];
        path.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        let output = Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .current_dir(self.path("work"))
            .arg("status")
            .args(args)
            .env("PATH", env::join_paths(path)?)
            .env("HOME", self.path("home"))
            .envs(variables.iter().copied())
            .output()?;
        Ok(Outcome {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8(output.stderr)?,
        })
    }
}

/// The variables a status needs: the viewer's token and the verdict App's ids.
const VARIABLES: [(&str, &str); 3] = [
    ("ASSAY_GITHUB_TOKEN", "ghs_test"),
    ("ASSAY_VERDICT_APP_ID", APP_ID),
    ("ASSAY_VERDICT_BOT_ID", BOT_ID),
];

const OWNERS: &str = "owners = [{ id = 11, login = \"owner\" }]\n";

#[test]
fn the_named_prs_status_is_read_through_gh_as_a_valid_document() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(OWNERS)?;

    let outcome = scene.status(&["--json", "--pr", "42"], &VARIABLES)?;

    assert_eq!(outcome.code, Some(0), "{outcome:?}");
    let document: Value = serde_json::from_str(&outcome.stdout)?;
    assert_eq!(
        (
            document["schema"].as_str(),
            document["pr"].as_u64(),
            document["check"].is_null()
        ),
        (Some("assay.status/1"), Some(42), true)
    );
    assert_eq!(
        scene.calls()?.first().map(String::as_str),
        Some(
            "call api --include --method GET --header Accept: application/vnd.github+json \
             --header X-GitHub-Api-Version: 2022-11-28 -- repos/Wet-Ink-Corporation/assay"
        )
    );
    Ok(())
}

#[test]
fn without_pr_the_checkouts_branch_names_the_pr() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(OWNERS)?;

    let outcome = scene.status(&[], &VARIABLES)?;

    assert_eq!(outcome.code, Some(0), "{outcome:?}");
    assert_eq!(
        outcome.stdout.lines().take(3).collect::<Vec<_>>(),
        [
            "assay status: Wet-Ink-Corporation/assay #42 (feature/status)",
            &format!("Head: {} (local HEAD matches)", scene.head),
            "Check: none yet",
        ]
    );
    Ok(())
}

#[test]
fn a_bundle_that_does_not_hash_to_its_pointer_is_refused_after_its_download()
-> Result<(), Box<dyn Error>> {
    let mut scene = Scene::new(OWNERS)?;
    scene.with_decided_check();

    let outcome = scene.status(&["--json", "--pr", "42"], &VARIABLES)?;

    assert_eq!((outcome.code, outcome.stdout.as_str()), (Some(2), ""));
    assert!(
        outcome
            .stderr
            .contains("the bundle does not hash to the evidence pointer"),
        "{}",
        outcome.stderr
    );
    let calls = scene.calls()?;
    let download = calls
        .iter()
        .find_map(|call| call.strip_prefix("call run download "))
        .ok_or("gh run download was called")?;
    let prefix = format!("--repo {REPO} --name {DECIDE} --dir ");
    let dest = download
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(" -- 7"))
        .ok_or_else(|| format!("download arguments: {download}"))?;
    assert!(!Path::new(dest).exists(), "{dest} was removed");
    Ok(())
}

#[test]
fn each_missing_or_invalid_variable_is_named_before_gh_runs() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(OWNERS)?;
    let cases = [
        ("ASSAY_GITHUB_TOKEN", ""),
        ("ASSAY_VERDICT_APP_ID", "0"),
        ("ASSAY_VERDICT_APP_ID", "+5"),
        ("ASSAY_VERDICT_BOT_ID", "x"),
    ];

    for (name, value) in cases {
        let mut variables = VARIABLES.to_vec();
        variables.retain(|&(known, _)| known != name);
        let missing = scene.status(&["--json", "--pr", "42"], &variables)?;
        variables.push((name, value));
        let invalid = scene.status(&["--json", "--pr", "42"], &variables)?;

        for outcome in [missing, invalid] {
            assert_eq!((outcome.code, outcome.stdout.as_str()), (Some(2), ""));
            assert!(
                outcome.stderr.contains(name),
                "{value:?}: {}",
                outcome.stderr
            );
        }
    }
    assert_eq!(scene.calls()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn an_origin_that_is_not_a_github_repository_is_refused_unechoed() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(OWNERS)?;
    let work = scene.path("work");
    let url = "https://someone:hunter2@example.com/assay.git";
    plain_git(&work, &["remote", "set-url", "origin", url])?;

    let outcome = scene.status(&["--json", "--pr", "42"], &VARIABLES)?;

    assert_eq!((outcome.code, outcome.stdout.as_str()), (Some(2), ""));
    assert!(outcome.stderr.contains("origin"), "{}", outcome.stderr);
    assert!(!outcome.stderr.contains("hunter2"), "{}", outcome.stderr);
    assert_eq!(scene.calls()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_detached_head_needs_a_pr_number() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(OWNERS)?;
    plain_git(&scene.path("work"), &["checkout", "--quiet", "--detach"])?;

    let detached = scene.status(&["--json"], &VARIABLES)?;
    let named = scene.status(&["--json", "--pr", "42"], &VARIABLES)?;

    assert_eq!((detached.code, detached.stdout.as_str()), (Some(2), ""));
    assert!(detached.stderr.contains("--pr"), "{}", detached.stderr);
    assert_eq!(named.code, Some(0), "{named:?}");
    Ok(())
}

#[test]
fn base_owners_that_do_not_parse_stop_the_status() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("owners = \"11\"\n")?;

    let outcome = scene.status(&["--json", "--pr", "42"], &VARIABLES)?;

    assert_eq!((outcome.code, outcome.stdout.as_str()), (Some(2), ""));
    assert!(
        outcome.stderr.contains(".assay/owners"),
        "{}",
        outcome.stderr
    );
    Ok(())
}

#[test]
fn a_default_branch_never_fetched_is_named() -> Result<(), Box<dyn Error>> {
    let mut scene = Scene::new(OWNERS)?;
    scene.answer(
        &format!("repos/{REPO}"),
        json!({"id": REPO_ID, "full_name": REPO, "default_branch": "trunk"}),
    );

    let outcome = scene.status(&["--json", "--pr", "42"], &VARIABLES)?;

    assert_eq!((outcome.code, outcome.stdout.as_str()), (Some(2), ""));
    assert!(
        outcome.stderr.contains("refs/remotes/origin/trunk"),
        "{}",
        outcome.stderr
    );
    Ok(())
}
