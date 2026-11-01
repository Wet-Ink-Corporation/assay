//! `assay decide-only decide --config <plugins-toml> --out <dir>` (#61 DO-11): the binary's
//! refusals before any token, run with a cleared environment and a `PATH` that finds no
//! program, so nothing can reach GitHub or the ledger.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::json;
use tempfile::TempDir;

const CONFIG: &str = "base/.assay/plugins.toml";
const DECIDE: [&str; 6] = [
    "decide-only",
    "decide",
    "--config",
    CONFIG,
    "--out",
    "redecide-out",
];
const SECRET: &str = "ghs_secret_value";

/// A working directory with base config, an empty `GITHUB_OUTPUT` and an empty `bin` as the
/// whole `PATH`.
struct Scene {
    root: TempDir,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = TempDir::new()?;
        fs::create_dir_all(root.path().join("base/.assay"))?;
        fs::create_dir_all(root.path().join("bin"))?;
        fs::create_dir_all(root.path().join("home"))?;
        fs::write(
            root.path().join(CONFIG),
            "[assay]\ndigest = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\nledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n",
        )?;
        fs::write(root.path().join("github-output"), "")?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// `assay decide-only decide` with only `variables`, besides `PATH` and `HOME`.
    fn redecide(&self, variables: &[(&str, String)]) -> Result<Output, Box<dyn Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .current_dir(self.root.path())
            .args(DECIDE)
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .envs(variables.iter().map(|(name, value)| (name, value)))
            .output()?)
    }

    /// The run's variables at `attempt`, guard's outputs scheduling PR 55 and `pr` as
    /// `ASSAY_PR` (unset when `None`).
    fn run(&self, attempt: &str, pr: Option<&str>) -> Result<Vec<(&str, String)>, Box<dyn Error>> {
        let output = self.path("github-output");
        let output = output.to_str().ok_or("a UTF-8 temporary path")?;
        let guard = json!({
            "guard": { "result": "success", "outputs": { "prs": "[55]", "trigger": "comment" } },
        });
        let mut variables = vec![
            ("GITHUB_RUN_ID", "39168909542".to_owned()),
            ("GITHUB_RUN_ATTEMPT", attempt.to_owned()),
            ("GITHUB_REPOSITORY_ID", "840213377".to_owned()),
            ("GITHUB_REPOSITORY", "Wet-Ink-Corporation/assay".to_owned()),
            ("GITHUB_OUTPUT", output.to_owned()),
            ("ASSAY_NEEDS", guard.to_string()),
        ];
        variables.extend(pr.map(|pr| ("ASSAY_PR", pr.to_owned())));
        Ok(variables)
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

    /// Asserts nothing was written: no output, no `--out`.
    fn assert_nothing_written(&self) -> Result<(), Box<dyn Error>> {
        assert_eq!(
            fs::read_to_string(self.path("github-output"))?,
            "",
            "no output"
        );
        assert!(!self.path("redecide-out").exists(), "no --out");
        Ok(())
    }
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_re_run_is_refused_before_any_token_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut variables = scene.run("2", Some("55"))?;
    variables.extend(Scene::tokens());

    let ran = scene.redecide(&variables)?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("attempt 2"), "{stderr}");
    assert!(!stderr.contains("TOKEN"), "no token is named: {stderr}");
    assert!(!stderr.contains(SECRET), "{stderr}");
    scene.assert_nothing_written()
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_pr_guard_did_not_schedule_is_refused_before_any_token_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    for (pr, refusal) in [
        (Some("56"), "ASSAY_PR 56 is not one of guard's prs"),
        (None, "ASSAY_PR is not set"),
        (Some("055"), "ASSAY_PR is not the PR this job decides"),
    ] {
        // No token is set: a refusal that read one would name it.
        let ran = scene.redecide(&scene.run("1", pr)?)?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{pr:?}: {stderr}");
        assert!(stderr.contains(refusal), "{pr:?}: {stderr}");
        assert!(!stderr.contains("TOKEN"), "no token is named: {stderr}");
        scene.assert_nothing_written()?;
    }
    Ok(())
}
