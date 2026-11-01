//! `assay admit --config <plugins-toml> --artifacts <dir> --out <dir>` (#60 ST-11, S1a): the
//! binary's refusals before any token is read, run with a cleared environment and a `PATH` that
//! finds no program, so nothing can reach GitHub or git.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

const CONFIG: &str = "base/.assay/plugins.toml";
const ARGS: [&str; 6] = [
    "--config",
    CONFIG,
    "--artifacts",
    "artifacts",
    "--out",
    "admit-out",
];

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
        fs::write(
            root.path().join(CONFIG),
            "[assay]\nledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n",
        )?;
        fs::write(root.path().join("github-output"), "")?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// `assay admit` with `args` and only `variables`, besides `PATH` and `HOME`.
    fn admit(&self, args: &[&str], variables: &[(&str, &str)]) -> Result<Output, Box<dyn Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_assay"))
            .env_clear()
            .current_dir(self.root.path())
            .arg("admit")
            .args(args)
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .envs(variables.iter().copied())
            .output()?)
    }
}

/// The run's variables at `attempt`, with every token set to `token`.
fn variables<'a>(output: &'a str, attempt: &'a str, token: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("GITHUB_RUN_ID", "39168909542"),
        ("GITHUB_RUN_ATTEMPT", attempt),
        ("GITHUB_REPOSITORY_ID", "840213377"),
        ("GITHUB_REPOSITORY", "Wet-Ink-Corporation/assay"),
        ("GITHUB_OUTPUT", output),
        ("ASSAY_NEEDS", "{}"),
        ("ASSAY_GITHUB_TOKEN", token),
        ("ASSAY_LEDGER_TOKEN", token),
        ("ASSAY_RUN_TOKEN", token),
    ]
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_re_run_is_refused_before_any_token_is_read() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let output = scene.path("github-output");
    let output = output.to_str().ok_or("a UTF-8 temporary path")?;

    let ran = scene.admit(&ARGS, &variables(output, "2", "ghs_secret_value"))?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("attempt 2"), "{stderr}");
    assert!(!stderr.contains("TOKEN"), "no token is named: {stderr}");
    assert!(!stderr.contains("ghs_secret_value"), "{stderr}");
    assert_eq!(fs::read_to_string(scene.path("github-output"))?, "");
    assert!(!scene.path("admit-out").exists());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn flags_other_than_config_artifacts_and_out_are_a_usage_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let output = scene.path("github-output");
    let output = output.to_str().ok_or("a UTF-8 temporary path")?;

    for args in [
        &ARGS[..4],
        &[
            "--config",
            CONFIG,
            "--out",
            "admit-out",
            "--artifacts",
            "artifacts",
        ][..],
        &[&ARGS[..], &["--extra"]].concat()[..],
    ] {
        let ran = scene.admit(args, &variables(output, "1", "ghs_secret_value"))?;

        let stderr = String::from_utf8(ran.stderr)?;
        assert_eq!(ran.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("usage: assay admit"), "{stderr}");
        assert!(!scene.path("admit-out").exists());
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn needs_without_guard_and_plan_are_refused_without_naming_a_token() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let output = scene.path("github-output");
    let output = output.to_str().ok_or("a UTF-8 temporary path")?;

    let ran = scene.admit(&ARGS, &variables(output, "1", "ghs_secret_value"))?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("admit: ASSAY_NEEDS: ASSAY_NEEDS has no entry for job guard"),
        "{stderr}"
    );
    assert!(!stderr.contains("ghs_secret_value"), "{stderr}");
    assert_eq!(fs::read_to_string(scene.path("github-output"))?, "");
    Ok(())
}
