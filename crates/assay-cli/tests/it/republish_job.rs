//! `assay decide-only publish --config <plugins-toml> --state <dir>` (#61 DO-12): the binary's
//! refusals before any token, run with a cleared environment and a `PATH` that finds no
//! program, so nothing can reach GitHub or the ledger.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::json;
use tempfile::TempDir;

const CONFIG: &str = "base/.assay/plugins.toml";
const PUBLISH: [&str; 6] = [
    "decide-only",
    "publish",
    "--config",
    CONFIG,
    "--state",
    "redecide-out",
];
const SECRET: &str = "ghs_secret_value";

/// A working directory with base config, decide's `--out` holding a silent state, and an empty
/// `bin` as the whole `PATH`.
struct Scene {
    root: TempDir,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = TempDir::new()?;
        fs::create_dir_all(root.path().join("base/.assay"))?;
        fs::create_dir_all(root.path().join("bin"))?;
        fs::create_dir_all(root.path().join("home"))?;
        fs::create_dir_all(root.path().join("redecide-out"))?;
        fs::write(
            root.path().join(CONFIG),
            "[assay]\ndigest = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\nledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n",
        )?;
        fs::write(
            root.path().join("redecide-out/state.json"),
            r#"{"verdict":"silent"}"#,
        )?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// `assay` with `args` and only `variables`, besides `PATH` and `HOME`.
    fn assay(&self, args: &[&str], variables: &[(&str, String)]) -> Result<Output, Box<dyn Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .current_dir(self.root.path())
            .args(args)
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .envs(variables.iter().map(|(name, value)| (name, value)))
            .output()?)
    }

    /// The run's variables at `attempt`, guard's outputs scheduling PR 55, `ASSAY_PR` 55 and
    /// `ASSAY_PERSIST` `skipped`, leaving out `without`.
    fn run(attempt: &str, without: &str) -> Vec<(&'static str, String)> {
        let guard = json!({
            "guard": { "result": "success", "outputs": { "prs": "[55]", "trigger": "comment" } },
        });
        [
            ("GITHUB_RUN_ID", "39168909542".to_owned()),
            ("GITHUB_RUN_ATTEMPT", attempt.to_owned()),
            ("GITHUB_REPOSITORY_ID", "840213377".to_owned()),
            ("GITHUB_REPOSITORY", "Wet-Ink-Corporation/assay".to_owned()),
            ("ASSAY_NEEDS", guard.to_string()),
            ("ASSAY_PR", "55".to_owned()),
            ("ASSAY_PERSIST", "skipped".to_owned()),
        ]
        .into_iter()
        .filter(|(name, _)| *name != without)
        .collect()
    }

    /// Every token and the verdict App's ids, as the redecide job holds them.
    fn tokens() -> Vec<(&'static str, String)> {
        vec![
            ("ASSAY_GITHUB_TOKEN", SECRET.to_owned()),
            ("ASSAY_LEDGER_TOKEN", SECRET.to_owned()),
            ("ASSAY_RUN_TOKEN", SECRET.to_owned()),
            ("ASSAY_VERDICT_APP_ID", "1234".to_owned()),
            ("ASSAY_VERDICT_BOT_ID", "5678".to_owned()),
        ]
    }
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_re_run_is_refused_before_any_token_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let without_tokens = Scene::run("2", "");
    // Two environments: the second is the first plus the tokens.
    let mut with_tokens = without_tokens.clone();
    with_tokens.extend(Scene::tokens());
    // Without tokens, a token read before the attempt would be refused as not set, by name.
    for variables in [without_tokens, with_tokens] {
        let ran = scene.assay(&PUBLISH, &variables)?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("decide-only publish"), "{stderr}");
        assert!(stderr.contains("attempt 2"), "{stderr}");
        assert!(!stderr.contains("TOKEN"), "no token is named: {stderr}");
        assert!(!stderr.contains(SECRET), "{stderr}");
        assert!(ran.stdout.is_empty(), "nothing published");
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_pr_guard_did_not_schedule_or_an_unknown_upload_outcome_is_refused_before_any_token()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut unscheduled = Scene::run("1", "ASSAY_PR");
    unscheduled.push(("ASSAY_PR", "56".to_owned()));
    let mut unknown = Scene::run("1", "ASSAY_PERSIST");
    unknown.push(("ASSAY_PERSIST", "succeeded".to_owned()));
    for (variables, refusal) in [
        (unscheduled, "ASSAY_PR 56 is not one of guard's prs"),
        (Scene::run("1", "ASSAY_PR"), "ASSAY_PR is not set"),
        (Scene::run("1", "ASSAY_PERSIST"), "ASSAY_PERSIST"),
        (unknown, "ASSAY_PERSIST"),
    ] {
        // No token is set: a refusal that read one would name it.
        let ran = scene.assay(&PUBLISH, &variables)?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{refusal}: {stderr}");
        assert!(stderr.contains(refusal), "{refusal}: {stderr}");
        assert!(!stderr.contains("TOKEN"), "no token is named: {stderr}");
        assert!(
            !stderr.contains("succeeded"),
            "the value is not echoed: {stderr}"
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn flags_other_than_the_steps_own_are_a_usage_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let usage = "usage: assay decide-only publish --config <plugins-toml> --state <dir>";
    for args in [
        &PUBLISH[..2],
        &PUBLISH[..5],
        &[&PUBLISH[..], &["--extra"]].concat(),
        &[
            "decide-only",
            "publish",
            "--state",
            "redecide-out",
            "--config",
            CONFIG,
        ],
    ] {
        let ran = scene.assay(args, &Scene::run("1", ""))?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(usage), "{args:?}: {stderr}");
    }
    Ok(())
}
