//! `assay admit --config <plugins-toml> --artifacts <dir> --out <dir>` (#60 ST-11, S1a): the
//! binary's refusals before any token is read, run with a cleared environment and a `PATH` that
//! finds no program, so nothing can reach GitHub or git.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use review_app::ArtifactJob;
use review_cli::{OutputName, RUN_FILE};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

use crate::admit_run::Planned;

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
            "[assay]\ndigest = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\nledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n",
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
        ("REVIEW_NEEDS", "{}"),
        ("REVIEW_GITHUB_TOKEN", token),
        ("REVIEW_LEDGER_TOKEN", token),
        ("REVIEW_RUN_TOKEN", token),
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

/// `toJSON(needs)` for a guard that wrote `planned`'s identity and a plan that wrote its outputs.
fn planned_needs(planned: &Planned) -> String {
    let outputs = |outputs: Vec<(OutputName, String)>| {
        outputs
            .into_iter()
            .map(|(name, value)| (name.as_str().to_owned(), Value::String(value)))
            .collect::<Map<String, Value>>()
    };
    json!({
        "guard": {"result": "success", "outputs": outputs(planned.identity.outputs())},
        "plan": {"result": "success", "outputs": outputs(planned.plan.outputs())},
    })
    .to_string()
}

/// `planned`'s document downloaded as plan's artifact under `<scene>/artifacts`, and the run's
/// variables (run 4242 of repository 42, as planned) with no token.
fn downloaded(scene: &Scene, planned: &Planned) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let artifact = planned
        .plan
        .artifacts()
        .get(&ArtifactJob::Plan)
        .ok_or("a plan artifact")?;
    let dir = scene.path("artifacts").join(artifact);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(RUN_FILE), &planned.document)?;
    let output = scene.path("github-output");
    let output = output.to_str().ok_or("a UTF-8 temporary path")?;
    Ok([
        ("GITHUB_RUN_ID", "4242"),
        ("GITHUB_RUN_ATTEMPT", "1"),
        ("GITHUB_REPOSITORY_ID", "42"),
        ("GITHUB_REPOSITORY", "Wet-Ink-Corporation/assay"),
        ("GITHUB_OUTPUT", output),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .chain([("REVIEW_NEEDS".to_owned(), planned_needs(planned))])
    .collect())
}

/// #347 (owner, 2026-11-02, T8 option B, wi-613db8): a run that spends nothing reads no
/// credential, so admit admits a draft with no token set and no base config to read.
#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_draft_is_admitted_without_any_token_or_base_config() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    fs::remove_file(scene.path(CONFIG))?;
    let planned = Planned::new(true)?;
    let variables = downloaded(&scene, &planned)?;

    let ran = scene.admit(&ARGS, &as_pairs(&variables))?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(scene.path("admit-out").join(RUN_FILE))?,
        planned.document
    );
    let written = fs::read_to_string(scene.path("github-output"))?;
    assert!(written.starts_with("admission=no-spend\n"), "{written}");
    assert!(written.ends_with("review=false\n"), "{written}");
    Ok(())
}

/// A run that spends still needs its tokens, read after the no-spend shortcut and before any
/// port: nothing is written without them.
#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_spend_without_its_tokens_is_refused_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let planned = Planned::new(false)?;
    let variables = downloaded(&scene, &planned)?;

    let ran = scene.admit(&ARGS, &as_pairs(&variables))?;

    let stderr = String::from_utf8(ran.stderr)?;
    assert_eq!(ran.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("admit: REVIEW_GITHUB_TOKEN is not set"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(scene.path("github-output"))?, "");
    assert!(!scene.path("admit-out").exists());
    Ok(())
}

/// `variables` as the borrowed pairs [`Scene::admit`] takes.
fn as_pairs(variables: &[(String, String)]) -> Vec<(&str, &str)> {
    variables
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
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
        stderr.contains("admit: REVIEW_NEEDS: REVIEW_NEEDS has no entry for job guard"),
        "{stderr}"
    );
    assert!(!stderr.contains("ghs_secret_value"), "{stderr}");
    assert_eq!(fs::read_to_string(scene.path("github-output"))?, "");
    Ok(())
}
