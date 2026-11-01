//! What `assay gate gates-static` (#60 ST-7) refuses before anything runs: a relative `--node`
//! or `--temper`, either inside `--head`, either missing, and a missing or invalid pinned
//! commit, run token, repository or `PATH`. Every case exits 2 with no git or node started and
//! nothing written, and a variable is named but never echoed. A valid pinned commit the
//! repository does not hold is a failed fetch: exit 2 after git ran, with no gate run and
//! nothing written.

use std::error::Error;
use std::path::PathBuf;

use crate::gate_static_scene::Scene;

/// The workflow's flags with `flag`'s value replaced by `value`.
fn with(scene: &Scene, flag: &str, value: impl Into<PathBuf>) -> Vec<(&'static str, PathBuf)> {
    let mut value = Some(value.into());
    scene
        .flags()
        .into_iter()
        .map(|(name, old)| match value.take_if(|_| name == flag) {
            Some(value) => (name, value),
            None => (name, old),
        })
        .collect()
}

/// Runs the job with `flags`: it must exit 2, nothing run or written, stderr starting `starts`.
fn refused(scene: &Scene, flags: &[(&str, PathBuf)], starts: &str) -> Result<(), Box<dyn Error>> {
    let output = scene.run_flags(flags)?;

    assert_eq!(output.status.code(), Some(2), "{flags:?}: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with(starts), "{flags:?}: {stderr}");
    assert!(scene.untouched()?, "{flags:?}: nothing runs");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_relative_node_or_temper_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[])?;

    refused(
        &scene,
        &with(&scene, "--node", "bin/node"),
        "assay: --node bin/node is not absolute",
    )?;
    refused(
        &scene,
        &with(&scene, "--temper", "pinned"),
        "assay: --temper pinned is not absolute",
    )
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_temper_or_node_inside_the_head_tree_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[])?;

    refused(
        &scene,
        &with(&scene, "--temper", scene.head.join(".claude/temper")),
        "assay: --temper ",
    )?;
    refused(
        &scene,
        &with(
            &scene,
            "--node",
            scene.head.join(".claude/temper/temper.mjs"),
        ),
        "assay: --node ",
    )
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn gates_static_without_node_or_temper_is_a_usage_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[])?;

    for dropped in ["--node", "--temper"] {
        let flags: Vec<_> = scene
            .flags()
            .into_iter()
            .filter(|(name, _)| *name != dropped)
            .collect();

        refused(&scene, &flags, "assay: usage: ")?;
    }
    Ok(())
}

/// Runs the job with `name` set to `value`, or unset when `None`: it must exit 2 naming
/// `name`, never showing `value`, with nothing run or written.
fn refused_variable(scene: &Scene, name: &str, value: Option<&str>) -> Result<(), Box<dyn Error>> {
    let output = scene.run(|command| {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    })?;

    assert_eq!(
        output.status.code(),
        Some(2),
        "{name}={value:?}: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(name), "{name} is named: {stderr}");
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        assert!(
            !stderr.contains(value),
            "{name}'s value is not shown: {stderr}"
        );
    }
    assert!(scene.untouched()?, "{name}={value:?}: nothing runs");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_or_invalid_pinned_commit_is_refused_before_anything_runs() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new(&[])?;
    let upper = scene.head_sha.to_uppercase();
    let short = scene.head_sha.get(1..).ok_or("a 40-hex SHA")?;

    for (name, value) in [
        ("ASSAY_BASE_SHA", None),
        ("ASSAY_HEAD_SHA", None),
        ("ASSAY_BASE_SHA", Some("")),
        ("ASSAY_BASE_SHA", Some("refs/heads/main")),
        ("ASSAY_HEAD_SHA", Some(upper.as_str())),
        ("ASSAY_HEAD_SHA", Some(short)),
    ] {
        refused_variable(&scene, name, value)?;
    }
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_or_invalid_token_repository_or_path_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[])?;

    for (name, value) in [
        ("ASSAY_RUN_TOKEN", None),
        ("ASSAY_RUN_TOKEN", Some("ghs_ with a space")),
        ("GITHUB_REPOSITORY", None),
        ("GITHUB_REPOSITORY", Some("not-a-repository")),
        ("PATH", None),
    ] {
        refused_variable(&scene, name, value)?;
    }
    Ok(())
}

/// A 40-hex commit the fixture repository does not hold.
const ABSENT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Runs the job with `name` set to [`ABSENT`]: it must exit 2 with stderr starting `starts`,
/// after git tried the fetch, with no gate run and nothing written.
fn unfetchable(scene: &Scene, name: &str, starts: &str) -> Result<(), Box<dyn Error>> {
    let output = scene.run(|command| {
        command.env(name, ABSENT);
    })?;

    assert_eq!(output.status.code(), Some(2), "{name}: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with(starts), "{name}: {stderr}");
    assert!(
        scene
            .recorded("git-bin/fetches")?
            .contains(&format!("{ABSENT}:refs/assay/fetched/{ABSENT}")),
        "{name}: the fetch was tried"
    );
    assert_eq!(scene.recorded("bin/node-env")?, "", "{name}: no gate ran");
    assert!(!scene.path("out").exists(), "{name}: no --out");
    assert_eq!(scene.recorded("output")?, "", "{name}: no output");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_head_commit_that_cannot_be_fetched_exits_2_and_runs_no_gate() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[])?;

    unfetchable(
        &scene,
        "ASSAY_HEAD_SHA",
        "assay: fetching the head commit: ",
    )
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_base_commit_that_cannot_be_fetched_exits_2_and_runs_no_gate() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new(&[])?;

    unfetchable(
        &scene,
        "ASSAY_BASE_SHA",
        "assay: fetching the base commit: ",
    )
}
