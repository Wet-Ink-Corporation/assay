//! The temper adapter's Z2s checks end to end (#63, `docs/architecture/03-contracts.md` §2.1):
//! fixture PRs built as git repositories and fetched into forge's object-only repository, read
//! through `ForgeRevisions`, with the pinned scan run by `ForgeSupervisor`.
//!
//! The real temper never runs here. A stand-in `node` checks it was invoked as the pinned
//! `node <pinned>/temper.mjs scan --all --json` from the head tree, then prints a recorded scan:
//! `tests/fixtures/temper-scan-expect.json` models temper 1.3.0's output (`scanSource` hits plus
//! the CLI's `file`) for a head whose `src/lib.rs` opens with an `#[expect]`.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assay_app::{Conclusion, ContractValidator, GateJob, GateOutcome, PluginsConfig, run_gates};
use assay_cli::{ForgeRevisions, ForgeSupervisor};
use assay_forge::git::{CommitSha, GitEnvironment, GitLimits, ObjectRepo};
use assay_forge::process::GroupKiller;
use assay_gates::{GateEnvironment, TemperAdapter, TemperSettings};
use tempfile::TempDir;

const BASELINE: &str = ".claude/temper-baseline.json";
const CONFIG: &str = ".claude/temper.json";

const TEMPER_JSON: &str = "{\n  \"skillGate\": true,\n  \"gate\": {\n    \"scan\": true,\n    \
                           \"stepTimeoutSec\": 900\n  },\n  \"scanner\": {\n    \"exempt\": []\n  }\n}\n";

/// The pinned temper copy, and the same files vendored in the fixture repository.
const TEMPER: [(&str, &str); 3] = [
    ("VERSION", "1.3.0\n"),
    ("lib/scan.mjs", "export function scanSource() {}\n"),
    ("temper.mjs", "#!/usr/bin/env node\n"),
];

const LIB: &str = "pub fn one() -> u8 {\n    1\n}\n";
const LIB_WITH_EXPECT: &str = "#[expect(clippy::too_many_lines, reason = \"one state machine\")]\n\
                               pub fn one() -> u8 {\n    1\n}\n\n// let x = 1;\n";

fn baseline(count: u64) -> String {
    format!(
        "{{\n  \"temper_baseline\": 1,\n  \"crates\": {{\n    \"fixture\": \".\"\n  }},\n  \
         \"totals\": {{\n    \"fixture\": {{\n      \"clippy::too_many_lines\": {count}\n    }}\n  }}\n}}\n"
    )
}

fn plugins() -> Result<PluginsConfig, Box<dyn Error>> {
    Ok(format!(
        "[assay]\ndigest = \"sha256:{}\"\n\n[[plugin]]\nname = \"temper\"\n\
         adapter = \"builtin:temper\"\npin = \"git:{}\"\nrequired = true\n",
        "dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b",
        "0123456789abcdef0123456789abcdef01234567",
    )
    .parse()?)
}

/// A fixture PR: a base commit with a clean crate, a baseline, a temper.json and a vendored
/// copy matching the pin, and a head commit with `head` written on top.
struct Pr {
    scratch: TempDir,
    work: PathBuf,
    repo: ObjectRepo,
    base: CommitSha,
    head: CommitSha,
}

impl Pr {
    fn new(head: &[(&str, &str)], scan: &str) -> Result<Self, Box<dyn Error>> {
        let scratch = tempfile::tempdir()?;
        let work = scratch.path().join("work");
        plain_git(scratch.path(), &["init", "--quiet", "work"])?;
        write(&work, "Cargo.toml", "[package]\nname = \"fixture\"\n")?;
        write(&work, "src/lib.rs", LIB)?;
        write(&work, BASELINE, &baseline(2))?;
        write(&work, CONFIG, TEMPER_JSON)?;
        for (path, text) in TEMPER {
            write(&work, &format!(".claude/temper/{path}"), text)?;
            write(&scratch.path().join("pinned"), path, text)?;
        }
        let base = commit(&work, "base")?;
        for (path, text) in head {
            write(&work, path, text)?;
        }
        let head = commit(&work, "head")?;
        let git_dir = scratch.path().join("objects.git");
        let repo = ObjectRepo::init(
            &git_dir,
            GitEnvironment::from_process_env()?,
            GitLimits::default(),
        )?;
        let destination = git_dir.to_str().ok_or("scratch path is not UTF-8")?;
        plain_git(
            &work,
            &["push", "--quiet", destination, "HEAD:refs/heads/pr"],
        )?;
        stand_in_node(scratch.path(), &work, scan)?;
        Ok(Self {
            scratch,
            work,
            repo,
            base,
            head,
        })
    }

    /// Runs `gates-static` through the gate use case: the outcome and each finding's rule.
    fn outcome(&self) -> Result<(GateOutcome, Vec<String>), Box<dyn Error>> {
        let bin = self.scratch.path().join("bin");
        let path = env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])?;
        let environment = GateEnvironment::from_vars([(OsString::from("PATH"), path)]);
        let settings = TemperSettings::new(
            self.scratch.path().join("pinned"),
            &self.work,
            &environment,
            "run-63",
        )?;
        let mut supervisor = ForgeSupervisor::new(GroupKiller::system()?);
        let revisions = ForgeRevisions::new(&self.repo, self.base.clone(), self.head.clone());
        let mut adapter = TemperAdapter::new(&mut supervisor, revisions, settings);
        let reports = run_gates(
            &plugins()?,
            GateJob::Static,
            &mut adapter,
            &ContractValidator::new()?,
        );
        let [report] = reports.as_slice() else {
            return Err(format!("expected one report, got {reports:?}").into());
        };
        let rules = report
            .document
            .as_ref()
            .and_then(|document| document.get("findings"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|finding| finding.get("rule")?.as_str().map(str::to_owned))
            .collect();
        Ok((report.outcome, rules))
    }
}

const FAILED: GateOutcome = GateOutcome::Concluded(Conclusion::Fail);
const PASSED: GateOutcome = GateOutcome::Concluded(Conclusion::Pass);

#[test]
#[cfg_attr(
    miri,
    ignore = "runs git and a stand-in node; Miri cannot spawn processes"
)]
fn a_pr_that_raises_the_baseline_fails() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new(&[(BASELINE, &baseline(3))], "[]")?;

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (FAILED, vec!["raised".to_owned()]), "2 -> 3");
    Ok(())
}

#[test]
#[cfg_attr(
    miri,
    ignore = "runs git and a stand-in node; Miri cannot spawn processes"
)]
fn a_pr_that_only_lowers_the_baseline_passes() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new(&[(BASELINE, &baseline(1))], "[]")?;

    let outcome = pr.outcome()?;

    assert_eq!(outcome, (PASSED, Vec::new()), "2 -> 1 is paying debt off");
    Ok(())
}

#[test]
#[cfg_attr(
    miri,
    ignore = "runs git and a stand-in node; Miri cannot spawn processes"
)]
fn a_pr_with_an_unallowlisted_expect_fails() -> Result<(), Box<dyn Error>> {
    let scan = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/temper-scan-expect.json"),
    )?;
    let pr = Pr::new(&[("src/lib.rs", LIB_WITH_EXPECT)], &scan)?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec!["exception".to_owned()]),
        "base has no .assay/exceptions.toml naming it; the soft hit is not this gate's"
    );
    Ok(())
}

#[test]
#[cfg_attr(
    miri,
    ignore = "runs git and a stand-in node; Miri cannot spawn processes"
)]
fn a_pr_that_drifts_the_gate_config_fails() -> Result<(), Box<dyn Error>> {
    let relaxed = TEMPER_JSON.replace("\"scan\": true", "\"scan\": false");
    let pr = Pr::new(&[(CONFIG, &relaxed)], "[]")?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (FAILED, vec!["gate-config".to_owned()]),
        "turning the gate's scan off"
    );
    Ok(())
}

#[test]
#[cfg_attr(
    miri,
    ignore = "runs git and a stand-in node; Miri cannot spawn processes"
)]
fn a_pr_that_edits_the_vendored_temper_gets_a_drift_finding() -> Result<(), Box<dyn Error>> {
    let pr = Pr::new(&[(".claude/temper/temper.mjs", "process.exit(0)\n")], "[]")?;

    let outcome = pr.outcome()?;

    assert_eq!(
        outcome,
        (PASSED, vec!["vendored-drift".to_owned()]),
        "the edited copy is never run, and its drift is reported"
    );
    Ok(())
}

/// Writes `bin/node` under `scratch`: it exits 2 unless invoked as the pinned scan from `head`,
/// and otherwise prints `scan`.
fn stand_in_node(scratch: &Path, head: &Path, scan: &str) -> Result<(), Box<dyn Error>> {
    let printed = scratch.join("scan.json");
    fs::write(&printed, scan)?;
    let script = scratch.join("pinned/temper.mjs");
    let node = scratch.join("bin/node");
    let body = format!(
        "#!/bin/sh\n\
         [ \"$#\" = 4 ] && [ \"$1\" = '{script}' ] && [ \"$2\" = scan ] && [ \"$3\" = --all ] \
         && [ \"$4\" = --json ] && [ \"$(pwd)\" = '{head}' ] || exit 2\n\
         exec /bin/cat '{printed}'\n",
        script = script.display(),
        head = head.display(),
        printed = printed.display(),
    );
    write(scratch, "bin/node", &body)?;
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn write(root: &Path, path: &str, text: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}

fn commit(work: &Path, message: &str) -> Result<CommitSha, Box<dyn Error>> {
    plain_git(work, &["add", "--all"])?;
    plain_git(work, &["commit", "--quiet", "--allow-empty", "-m", message])?;
    Ok(plain_git(work, &["rev-parse", "HEAD"])?.trim().parse()?)
}

/// Runs git the way a developer would, isolated from the machine's config. Test setup only.
fn plain_git(cwd: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let mut git = Command::new("git");
    git.env_clear();
    if let Some(path) = env::var_os("PATH") {
        git.env("PATH", path);
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
