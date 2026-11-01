//! `assay decide --config <plugins-toml> --artifacts <dir> --out <dir>` and `assay publish
//! --config <plugins-toml> --state <dir>` (#60 ST-14, Z4): the binary's refusals before any
//! port, run with a cleared environment and a `PATH` that finds no program, so nothing can
//! reach GitHub or git.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = "base/.assay/plugins.toml";
const DECIDE: [&str; 7] = [
    "decide",
    "--config",
    CONFIG,
    "--artifacts",
    "artifacts",
    "--out",
    "decide-out",
];
const PUBLISH: [&str; 5] = ["publish", "--config", CONFIG, "--state", "decide-out"];
const SECRET: &str = "ghs_secret_value";

/// A working directory with base config and an empty `bin` as the whole `PATH`.
struct Scene {
    root: TempDir,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = TempDir::new()?;
        fs::create_dir_all(root.path().join("base/.assay"))?;
        fs::create_dir_all(root.path().join("bin"))?;
        fs::create_dir_all(root.path().join("home"))?;
        fs::create_dir_all(root.path().join("artifacts"))?;
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

    /// The run's variables at `attempt`, every token set, and the verdict App's ids unless
    /// `without` names one.
    fn variables(
        &self,
        attempt: &str,
        without: &str,
    ) -> Result<Vec<(&'static str, String)>, Box<dyn Error>> {
        let output = self.path("github-output");
        let output = output.to_str().ok_or("a UTF-8 temporary path")?;
        let all = [
            ("GITHUB_RUN_ID", "39168909542".to_owned()),
            ("GITHUB_RUN_ATTEMPT", attempt.to_owned()),
            ("GITHUB_REPOSITORY_ID", "840213377".to_owned()),
            ("GITHUB_REPOSITORY", "Wet-Ink-Corporation/assay".to_owned()),
            ("GITHUB_OUTPUT", output.to_owned()),
            ("ASSAY_NEEDS", needs()),
            ("ASSAY_GITHUB_TOKEN", SECRET.to_owned()),
            ("ASSAY_LEDGER_TOKEN", SECRET.to_owned()),
            ("ASSAY_RUN_TOKEN", SECRET.to_owned()),
            ("ASSAY_VERDICT_APP_ID", "1234".to_owned()),
            ("ASSAY_VERDICT_BOT_ID", "5678".to_owned()),
            ("ASSAY_PERSIST", "skipped".to_owned()),
        ];
        Ok(all
            .into_iter()
            .filter(|(name, _)| *name != without)
            .collect())
    }
}

/// Every job decide needs: guard's outputs for a ready PR, and every later job skipped.
fn needs() -> String {
    let skipped = || json!({ "result": "skipped", "outputs": {} });
    let commit = "3333333333333333333333333333333333333333";
    let guard: Value = json!({
        "result": "success",
        "outputs": {
            "run": "true", "pr": "42", "head_sha": commit, "base_sha": commit,
            "base_ref": "main", "draft": "false", "author_id": "7", "sender_id": "8",
            "trigger": "opened", "default_branch": "main",
        },
    });
    json!({
        "guard": guard,
        "open": skipped(),
        "plan": skipped(),
        "gates-static": skipped(),
        "gates-lint": skipped(),
        "gates-test": skipped(),
        "admit": skipped(),
        "review": skipped(),
        "release": skipped(),
    })
    .to_string()
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_re_run_is_refused_before_any_token_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    for args in [&DECIDE[..], &PUBLISH[..]] {
        let ran = scene.assay(args, &scene.variables("2", "")?)?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("attempt 2"), "{args:?}: {stderr}");
        assert!(!stderr.contains("TOKEN"), "no token is named: {stderr}");
        assert!(!stderr.contains(SECRET), "{stderr}");
        assert_eq!(fs::read_to_string(scene.path("github-output"))?, "");
        assert!(!scene.path("decide-out").exists());
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn the_verdict_apps_ids_are_required() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    for args in [&DECIDE[..], &PUBLISH[..]] {
        for name in ["ASSAY_VERDICT_APP_ID", "ASSAY_VERDICT_BOT_ID"] {
            let ran = scene.assay(args, &scene.variables("1", name)?)?;

            let stderr = String::from_utf8(ran.stderr)?;
            assert_eq!(ran.status.code(), Some(2), "{args:?}: {stderr}");
            assert!(
                stderr.contains(&format!("{name} is not set")),
                "{args:?}: {stderr}"
            );
            assert!(!stderr.contains(SECRET), "{stderr}");
            assert!(!scene.path("decide-out").exists());
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_upload_outcome_other_than_the_four_words_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    for persist in [None, Some("succeeded")] {
        let mut variables = scene.variables("1", "ASSAY_PERSIST")?;
        if let Some(persist) = persist {
            variables.push(("ASSAY_PERSIST", persist.to_owned()));
        }

        let ran = scene.assay(&PUBLISH, &variables)?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{persist:?}: {stderr}");
        assert!(stderr.contains("publish: ASSAY_PERSIST"), "{stderr}");
        assert!(
            !stderr.contains("succeeded"),
            "the value is not echoed: {stderr}"
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn flags_other_than_the_commands_own_are_a_usage_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let cases: [(&[&str], &str); 4] = [
        (&DECIDE[..5], "usage: assay decide"),
        (&[&DECIDE[..], &["--extra"]].concat(), "usage: assay decide"),
        (&PUBLISH[..3], "usage: assay publish"),
        (
            &["publish", "--state", "decide-out", "--config", CONFIG],
            "usage: assay publish",
        ),
    ];
    for (args, usage) in cases {
        let ran = scene.assay(args, &scene.variables("1", "")?)?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(usage), "{args:?}: {stderr}");
        assert!(!scene.path("decide-out").exists());
    }
    Ok(())
}
