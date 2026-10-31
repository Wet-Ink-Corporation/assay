//! `assay plan --out <dir>` (#60 ST-5) as the binary: every refusal before the network is
//! reached exits 2 with nothing written and no `gh` started, and the one `gh` read (the PR's
//! file list) goes out with `ASSAY_RUN_TOKEN` and no other token. The environment is cleared
//! and holds only the Actions variables, guard's outputs and a stand-in `gh` on `PATH` that
//! records its calls and token and answers from a file; no git fetch runs (no network).

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;

use crate::support::write_executable;

const REPO: &str = "Wet-Ink-Corporation/assay";
const HEAD: &str = "1111111111111111111111111111111111111111";
const BASE: &str = "2222222222222222222222222222222222222222";
const RUN_TOKEN: &str = "ghs_runtoken";

/// How the binary ended.
#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    stderr: String,
    /// `GITHUB_OUTPUT`'s bytes afterwards.
    output: String,
}

/// A scratch directory (resolved, outside any checkout) with an empty `GITHUB_OUTPUT` and a
/// `bin/` holding the stand-in `gh`.
struct Scene {
    _dir: TempDir,
    root: PathBuf,
}

/// guard's outputs for PR 55, as `toJSON(needs)` gives them.
fn guard_needs() -> String {
    json!({"guard": {"result": "success", "outputs": {
        "run": "true", "pr": "55", "head_sha": HEAD, "base_sha": BASE, "base_ref": "main",
        "draft": "false", "author_id": "7", "sender_id": "7", "trigger": "opened",
        "default_branch": "main",
    }}})
    .to_string()
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let root = fs::canonicalize(dir.path())?;
        fs::write(root.join("output"), "")?;
        fs::create_dir(root.join("bin"))?;
        let script = format!(
            "#!/bin/sh\ndir=${{0%/*}}\n\
             {{ printf call; for arg; do printf ' %s' \"$arg\"; done; printf '\\n'; }} >> \"$dir/calls\"\n\
             printf '%s\\n' \"$GH_TOKEN\" >> \"$dir/tokens\"\n\
             for last; do :; done\ncase \"$last\" in\n\
             'repos/{REPO}/pulls/55') printf 'HTTP/2.0 200 OK\\n\\r\\n{{\"number\":56,\"changed_files\":1}}' ;;\n\
             *) printf 'HTTP/2.0 404 Not Found\\n\\r\\n{{\"message\":\"Not Found\"}}'; exit 1 ;;\n\
             esac\n"
        );
        write_executable(&root.join("bin/gh"), &script)?;
        Ok(Self { _dir: dir, root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// What the stand-in `gh` recorded in `name`, empty when it never ran.
    fn recorded(&self, name: &str) -> Result<String, Box<dyn Error>> {
        let file = self.path("bin").join(name);
        Ok(if file.exists() {
            fs::read_to_string(file)?
        } else {
            String::new()
        })
    }

    /// `assay plan` with `args`, the Actions variables, guard's outputs, the run token and the
    /// stand-in `gh` first on `PATH`, then `changed` (an empty value unsets the variable).
    fn plan(&self, args: &[&str], changed: &[(&str, &str)]) -> Result<Ran, Box<dyn Error>> {
        let bin = self.path("bin");
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let needs = guard_needs();
        let output = self.path("output");
        let output = output.to_str().ok_or("a UTF-8 scratch path")?;
        let mut vars = vec![
            ("GITHUB_RUN_ID", "4242"),
            ("GITHUB_RUN_ATTEMPT", "1"),
            ("GITHUB_REPOSITORY_ID", "42"),
            ("GITHUB_REPOSITORY", REPO),
            ("GITHUB_OUTPUT", output),
            ("ASSAY_NEEDS", needs.as_str()),
            ("ASSAY_RUN_TOKEN", RUN_TOKEN),
            ("PATH", path.as_str()),
        ];
        for &(name, value) in changed {
            vars.retain(|&(own, _)| own != name);
            if !value.is_empty() {
                vars.push((name, value));
            }
        }
        let ran = Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .current_dir(&self.root)
            .arg("plan")
            .args(args)
            .envs(vars)
            .output()?;
        Ok(Ran {
            code: ran.status.code(),
            stderr: String::from_utf8(ran.stderr)?,
            output: fs::read_to_string(self.path("output"))?,
        })
    }

    /// `assay plan --out plan-out` with `changed`.
    fn plan_out(&self, changed: &[(&str, &str)]) -> Result<Ran, Box<dyn Error>> {
        self.plan(&["--out", "plan-out"], changed)
    }

    /// Exit 2, nothing appended to `GITHUB_OUTPUT`, no `plan-out` and no `gh` started.
    fn refused_before_gh(&self, ran: &Ran) -> Result<(), Box<dyn Error>> {
        assert_eq!(
            (
                ran.code,
                ran.output.as_str(),
                self.recorded("calls")?.as_str()
            ),
            (Some(2), "", ""),
            "{ran:?}"
        );
        assert!(!self.path("plan-out").exists(), "{ran:?}");
        Ok(())
    }
}

#[test]
fn anything_but_one_out_flag_is_a_usage_error() -> Result<(), Box<dyn Error>> {
    let cases: [&[&str]; 5] = [
        &[],
        &["--out"],
        &["--out", "a", "--out", "b"],
        &["--config", "base/.assay/plugins.toml"],
        &["--out", "a", "extra"],
    ];
    for args in cases {
        let scene = Scene::new()?;

        let ran = scene.plan(args, &[])?;

        scene.refused_before_gh(&ran)?;
        assert!(
            ran.stderr
                .starts_with("assay: usage: assay plan --out <dir>"),
            "{ran:?}"
        );
    }
    Ok(())
}

#[test]
fn a_missing_or_invalid_run_variable_is_named_and_exits_2() -> Result<(), Box<dyn Error>> {
    for (name, value) in [("GITHUB_RUN_ID", ""), ("GITHUB_RUN_ATTEMPT", "01")] {
        let scene = Scene::new()?;

        let ran = scene.plan_out(&[(name, value)])?;

        scene.refused_before_gh(&ran)?;
        assert!(ran.stderr.contains(name), "{ran:?}");
    }
    Ok(())
}

#[test]
fn without_github_output_plan_cannot_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let ran = scene.plan_out(&[("GITHUB_OUTPUT", "")])?;

    scene.refused_before_gh(&ran)?;
    assert!(ran.stderr.contains("GITHUB_OUTPUT"), "{ran:?}");
    Ok(())
}

#[test]
fn guards_outputs_missing_or_refused_exit_2() -> Result<(), Box<dyn Error>> {
    let failed = guard_needs().replace("\"success\"", "\"failure\"");
    for value in ["", "{}", failed.as_str()] {
        let scene = Scene::new()?;

        let ran = scene.plan_out(&[("ASSAY_NEEDS", value)])?;

        scene.refused_before_gh(&ran)?;
        assert!(ran.stderr.contains("ASSAY_NEEDS"), "{value}: {ran:?}");
    }
    Ok(())
}

#[test]
fn an_out_that_exists_is_refused_before_gh() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    fs::create_dir(scene.path("plan-out"))?;

    let ran = scene.plan_out(&[])?;

    assert_eq!(
        (
            ran.code,
            ran.output.as_str(),
            scene.recorded("calls")?.as_str()
        ),
        (Some(2), "", ""),
        "{ran:?}"
    );
    assert!(ran.stderr.contains("already exists"), "{ran:?}");
    Ok(())
}

#[test]
fn an_out_inside_a_checkout_is_refused_before_gh() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    fs::create_dir(scene.path(".git"))?;

    let ran = scene.plan_out(&[])?;

    scene.refused_before_gh(&ran)?;
    assert!(ran.stderr.contains("inside the checkout"), "{ran:?}");
    Ok(())
}

#[test]
fn a_missing_or_invalid_run_token_is_named_never_shown() -> Result<(), Box<dyn Error>> {
    for value in ["", "ghs_has a space"] {
        let scene = Scene::new()?;

        let ran = scene.plan_out(&[("ASSAY_RUN_TOKEN", value)])?;

        scene.refused_before_gh(&ran)?;
        assert!(ran.stderr.contains("ASSAY_RUN_TOKEN"), "{ran:?}");
        assert!(
            value.is_empty() || !ran.stderr.contains(value),
            "the token is never shown: {ran:?}"
        );
    }
    Ok(())
}

#[test]
fn gh_reads_the_file_list_with_the_run_token_and_a_refused_answer_exits_2()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    // The other jobs' tokens are set too: plan must not send them.
    let ran = scene.plan_out(&[
        ("ASSAY_GITHUB_TOKEN", "ghs_verdict"),
        ("ASSAY_LEDGER_TOKEN", "ghs_ledger"),
    ])?;

    assert_eq!(
        (ran.code, ran.output.as_str()),
        (Some(2), ""),
        "GitHub answered for PR 56: {ran:?}"
    );
    assert!(
        ran.stderr.starts_with("assay: plan: the PR's file list: "),
        "{ran:?}"
    );
    assert_eq!(
        (scene.recorded("calls")?, scene.recorded("tokens")?),
        (
            format!(
                "call api --include --method GET --header Accept: application/vnd.github+json \
                 --header X-GitHub-Api-Version: 2022-11-28 -- repos/{REPO}/pulls/55\n"
            ),
            format!("{RUN_TOKEN}\n")
        )
    );
    assert!(!scene.path("plan-out").exists(), "{ran:?}");
    Ok(())
}
