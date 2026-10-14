//! `assay check --local` (#107, 02 §6) end to end on a fixture repository with a dirty working
//! tree, an `exec` gate and a stand-in `claude` (the real CLI never runs); its refusals; and
//! that nothing in CI reads the report it writes (R-10, P9).

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

const DIGEST: &str = "sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// A required `exec` gate, `unit`, in the test job.
fn plugins_toml() -> String {
    format!(
        "[assay]\ndigest = \"{DIGEST}\"\nprotected_bases = [\"main\"]\n\n\
         [[plugin]]\nname = \"unit\"\nadapter = \"exec\"\nexec = [\"./gate.sh\"]\n\
         job = \"test\"\nrequired = true\n"
    )
}

/// The `unit` gate: passes, with its document at `ASSAY_FINDINGS_OUT`.
const GATE: &str = "#!/bin/sh\n\
printf '{\"schema\":\"assay.findings/1\",\"source\":{\"kind\":\"gate\",\"name\":\"unit\",\
\"version\":\"local\",\"run_key\":\"%s\"},\"conclusion\":\"pass\",\"findings\":[]}' \
\"$ASSAY_RUN_KEY\" > \"$ASSAY_FINDINGS_OUT\"\n";

/// A repository whose `main` is also `origin/main`, with a dirty working tree, and a stand-in
/// `claude` that records the tree it was given.
pub(crate) struct Scene {
    root: TempDir,
}

impl Scene {
    pub(crate) fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let work = root.path().join("work");
        plain_git(root.path(), &["init", "--quiet", "-b", "main", "work"])?;
        write(&work, ".assay/plugins.toml", &plugins_toml())?;
        write(&work, ".gitignore", ".env\n")?;
        write(&work, "gate.sh", GATE)?;
        fs::set_permissions(work.join("gate.sh"), fs::Permissions::from_mode(0o755))?;
        write(&work, "src/lib.rs", "pub fn one() -> u32 {\n    1\n}\n")?;
        plain_git(&work, &["add", "--all"])?;
        plain_git(&work, &["commit", "--quiet", "-m", "base"])?;
        plain_git(&work, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
        write(&work, "src/lib.rs", "pub fn one() -> u32 {\n    2\n}\n")?;
        write(&work, "src/new.rs", "pub fn two() {}\n")?;
        write(&work, ".env", "SECRET=1\n")?;
        let scene = Self { root };
        scene.stand_in(&json!({"findings": []}))?;
        fs::create_dir(scene.path("home"))?;
        Ok(scene)
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Writes the stand-in `claude`: it drains its prompt, lists the tree `--add-dir` names
    /// into `seen-tree`, and prints a clean init and a result holding `output`.
    fn stand_in(&self, output: &Value) -> Result<(), Box<dyn Error>> {
        let init = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../assay-runner/tests/fixtures/stream/ok.jsonl"),
        )?;
        let init = init.lines().next().ok_or("the fixture stream is empty")?;
        let result = json!({
            "type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
            "total_cost_usd": 0.05, "result": "done", "structured_output": output,
            "session_id": "5f0c1d2e-0000-4000-8000-000000000001",
        });
        let stream = self.path("stream.jsonl");
        fs::write(&stream, format!("{init}\n{result}\n"))?;
        let script = format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --add-dir ]; then dir=\"$2\"; fi\n  \
             shift\ndone\n/bin/cat > /dev/null\n(cd \"$dir\" && /usr/bin/find . -type f | \
             /usr/bin/sort) > '{seen}'\n/bin/cat '{stream}'\n",
            seen = self.path("seen-tree").display(),
            stream = stream.display(),
        );
        let program = self.path("claude");
        fs::write(&program, script)?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    pub(crate) fn check(&self, args: &[&str]) -> Result<Outcome, Box<dyn Error>> {
        self.check_in(&self.path("work"), args)
    }

    fn check_in(&self, cwd: &Path, args: &[&str]) -> Result<Outcome, Box<dyn Error>> {
        self.run(cwd, env::var_os("PATH"), args)
    }

    /// `assay check <args>` from `cwd`, with only `HOME` and `path` as `PATH`.
    fn run(
        &self,
        cwd: &Path,
        path: Option<OsString>,
        args: &[&str],
    ) -> Result<Outcome, Box<dyn Error>> {
        self.assay(cwd, path, &[&["check"], args].concat())
    }

    /// `assay status <args>` from the working tree, with only `HOME` and `PATH`.
    pub(crate) fn status(&self, args: &[&str]) -> Result<Outcome, Box<dyn Error>> {
        self.assay(
            &self.path("work"),
            env::var_os("PATH"),
            &[&["status"], args].concat(),
        )
    }

    /// `assay <args>` from `cwd`, with only `HOME` and `path` as `PATH`.
    fn assay(
        &self,
        cwd: &Path,
        path: Option<OsString>,
        args: &[&str],
    ) -> Result<Outcome, Box<dyn Error>> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
        command.env_clear().current_dir(cwd).args(args);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let output = command.env("HOME", self.path("home")).output()?;
        Ok(Outcome {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8(output.stderr)?,
        })
    }

    pub(crate) fn claude(&self) -> Result<String, Box<dyn Error>> {
        Ok(self
            .path("claude")
            .to_str()
            .ok_or("scratch path is not UTF-8")?
            .to_owned())
    }
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[test]
fn a_dirty_tree_is_checked_and_its_report_written_under_the_git_dir() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;
    let work = scene.path("work");
    let status_before = plain_git(&work, &["status", "--porcelain"])?;

    let outcome = scene.check(&["--local", "--claude", &scene.claude()?])?;

    let tree = snapshot_tree(&work)?;
    let report_path = work.join(".git/assay").join(format!("{tree}.json"));
    let report: Value = serde_json::from_slice(&fs::read(&report_path)?)?;
    assert_eq!(
        (&report["schema"], &report["advisory"], &report["tree"]),
        (&json!("assay.local-check/2"), &json!(true), &json!(tree)),
        "{outcome:?}"
    );
    assert_eq!(
        report["gates"],
        json!([{"name": "unit", "job": "gates-test", "required": true, "outcome": "pass",
                "findings": []}])
    );
    assert_eq!(report["review"]["status"], "completed", "{report:#}");
    assert!(
        outcome
            .stdout
            .starts_with("assay check --local: advisory, CI never reads this report\n"),
        "{outcome:?}"
    );
    assert!(
        outcome
            .stdout
            .contains(&format!("report: {}", report_path.display()))
    );
    assert_eq!(
        plain_git(&work, &["status", "--porcelain"])?,
        status_before,
        "the developer's index and tree are untouched"
    );
    assert_eq!(
        fs::read_to_string(scene.path("seen-tree"))?,
        "./.assay/plugins.toml\n./.gitignore\n./gate.sh\n./src/lib.rs\n./src/new.rs\n",
        "the model reads the snapshot: the edit and the untracked file, never the ignored .env"
    );
    Ok(())
}

#[test]
fn a_file_the_developer_ignores_globally_never_reaches_the_model() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let work = scene.path("work");
    write(&scene.path("home"), ".config/git/ignore", ".idea/\n")?;
    write(&work, ".idea/workspace.xml", "<private/>\n")?;

    let outcome = scene.check(&["--local", "--claude", &scene.claude()?])?;

    assert_eq!(
        fs::read_to_string(scene.path("seen-tree"))?,
        "./.assay/plugins.toml\n./.gitignore\n./gate.sh\n./src/lib.rs\n./src/new.rs\n",
        "git's default global excludes file applies to the snapshot: {outcome:?}"
    );
    Ok(())
}

#[test]
fn without_package_sets_a_rust_change_is_full_tier_and_the_decision_says_what_is_missing()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let outcome = scene.check(&["--local", "--claude", &scene.claude()?])?;

    let work = scene.path("work");
    let report_path = work
        .join(".git/assay")
        .join(format!("{}.json", snapshot_tree(&work)?));
    let report: Value = serde_json::from_slice(&fs::read(&report_path)?)?;
    // Local mode computes no package sets (#65), which plan reads as exec surface: a known gap
    // until it does, recorded in 02 §6.
    assert_eq!(report["tier"], "full");
    assert_eq!(report["decision"]["conclusion"], "failure");
    assert_eq!(
        outcome.code,
        Some(1),
        "a failing decision exits 1: {outcome:?}"
    );
    let degraded = report["decision"]["degraded"]
        .as_array()
        .ok_or("degraded is a list")?;
    assert!(
        degraded.contains(&json!({"role": "bugs", "reason": "missing"})),
        "{degraded:?}"
    );
    Ok(())
}

#[test]
fn a_tree_that_is_not_a_repository_root_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let outcome = scene.check_in(
        &scene.path("work/src"),
        &["--local", "--claude", &scene.claude()?],
    )?;

    assert_eq!(outcome.code, Some(2), "{outcome:?}");
    assert!(outcome.stderr.contains("has no .git"), "{outcome:?}");
    assert!(!scene.path("work/.git/assay").exists());
    Ok(())
}

#[test]
fn a_base_branch_that_was_never_fetched_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let outcome = scene.check(&["--local", "--base", "release", "--claude", &scene.claude()?])?;

    assert_eq!(outcome.code, Some(2));
    assert_eq!(
        outcome.stderr,
        "assay: refs/remotes/origin/release is not a commit: fetch it, or pass --base <branch>\n"
    );
    Ok(())
}

#[test]
fn malformed_arguments_are_usage_errors() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let claude = scene.claude()?;
    let usage = "assay: usage: assay check --local [--base <branch>] [--claude <program>] \
                 [--events <path|->], run from the repository root\n";

    for args in [
        &[][..],
        &["--remote"],
        &["--local", "--base"],
        &["--local", "--frobnicate", "x"],
        &["--local", "--base", "main", "--base", "main"],
        &[
            "--local",
            "--claude",
            claude.as_str(),
            "--claude",
            claude.as_str(),
        ],
    ] {
        let outcome = scene.check(args)?;
        assert_eq!(
            (outcome.code, outcome.stderr.as_str()),
            (Some(2), usage),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn a_base_that_is_not_a_plain_branch_name_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    for base in ["main^", "../main", "-x", "main..x", ".hidden", "a//b", ""] {
        let outcome = scene.check(&["--local", "--base", base])?;
        assert_eq!(
            (outcome.code, outcome.stderr),
            (
                Some(2),
                format!("assay: the base {base:?} is not a plain branch name\n")
            ),
        );
    }
    Ok(())
}

#[test]
fn without_claude_on_path_the_program_must_be_given() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let empty = scene.path("home");

    let outcome = scene.run(
        &scene.path("work"),
        Some(empty.into_os_string()),
        &["--local"],
    )?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            "assay: no claude on PATH: pass --claude <absolute path>\n"
        )
    );
    Ok(())
}

#[test]
fn a_relative_claude_program_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let outcome = scene.check(&["--local", "--claude", "bin/claude"])?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            "assay: the claude program bin/claude is not an absolute path\n"
        )
    );
    Ok(())
}

/// No workflow and no production module but local mode's own names the local report, and no
/// workflow runs local mode, its event stream or its status (`--local`, `--events`, #130), so no
/// CI path can read any of them; `assay_app`'s `ci_refuses_the_local_report_as_every_contract`
/// shows the contract validator refuses the report too.
#[test]
fn nothing_in_ci_names_the_local_report() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let allowed = [
        "crates/assay-cli/src/check.rs",
        "crates/assay-app/src/check/report.rs",
    ];
    let mut naming = Vec::new();
    for dir in [".github", "crates"] {
        for file in files(&root.join(dir))? {
            let relative = file.strip_prefix(&root)?.to_string_lossy().into_owned();
            let production = dir == ".github" || relative.contains("/src/");
            if !production || allowed.contains(&relative.as_str()) {
                continue;
            }
            let text = fs::read(&file)?;
            let text = String::from_utf8_lossy(&text);
            let names: &[&str] = if dir == ".github" {
                &["assay.local-check", ".git/assay", "--local", "--events"]
            } else {
                &["assay.local-check", ".git/assay"]
            };
            if names.iter().any(|name| text.contains(name)) {
                naming.push(relative);
            }
        }
    }
    assert_eq!(naming, Vec::<String>::new());
    Ok(())
}

/// Every file under `dir`, recursively, symlinks not followed.
fn files(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The tree `git add --all` would write for `work`, computed in a throwaway index.
pub(crate) fn snapshot_tree(work: &Path) -> Result<String, Box<dyn Error>> {
    let index = work.join(".git/test-snapshot-index");
    let index_env = index.to_str().ok_or("scratch path is not UTF-8")?;
    plain_git_env(work, &["read-tree", "HEAD"], index_env)?;
    plain_git_env(work, &["add", "--all"], index_env)?;
    let tree = plain_git_env(work, &["write-tree"], index_env)?;
    fs::remove_file(index)?;
    Ok(tree.trim().to_owned())
}

pub(crate) fn write(root: &Path, path: &str, contents: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

fn plain_git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    plain_git_env(cwd, args, "")
}

/// Runs git as a developer would, isolated from the machine's config, with `index` as its
/// index file when not empty. Test setup only.
fn plain_git_env(cwd: &Path, args: &[&str], index: &str) -> Result<String, Box<dyn Error>> {
    let mut git = Command::new("git");
    git.env_clear();
    if let Some(path) = env::var_os("PATH") {
        git.env("PATH", path);
    }
    if !index.is_empty() {
        git.env("GIT_INDEX_FILE", index);
    }
    let output = git
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}
