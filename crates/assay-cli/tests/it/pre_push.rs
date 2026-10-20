//! CP1 at the push (#108, 04 §9): `assay pre-push` on the fixture repository of `check.rs`,
//! and `plugin/hooks/pre-push` installed in it as git's pre-push hook, against a bare remote,
//! with a stand-in `weighin` printing weighin 0.9.0's recorded `open --repo --json` (the real
//! binary never runs). A seeded blocking item blocks the push and `--no-verify` bypasses it;
//! an untriaged item warns and the push goes through.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::check::{Scene, plain_git};
use crate::support::write_executable;

/// weighin 0.9.0's recorded output: `feat-x` has a blocking item (`wi-7529b9`) and a pending
/// one (`wi-b83949`), `other` a pending one only (`wi-216116`).
fn recorded() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assay-app/tests/fixtures/weigh-in/open-0.9.0.json")
}

/// The shipped hook directory.
fn hooks() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin/hooks")
}

/// The fixture scene on a new `branch` with a stand-in `weighin` that prints the recording.
fn scene_on(branch: &str) -> Result<(Scene, PathBuf), Box<dyn Error>> {
    let scene = Scene::new()?;
    plain_git(&scene.path("work"), &["checkout", "--quiet", "-b", branch])?;
    let weighin = scene.path("weighin");
    write_executable(
        &weighin,
        &format!("#!/bin/sh\n/bin/cat '{}'\n", recorded().display()),
    )?;
    Ok((scene, weighin))
}

#[test]
fn pre_push_reads_a_tree_local_mode_would_refuse() -> Result<(), Box<dyn Error>> {
    let (scene, weighin) = scene_on("other")?;
    let work = scene.path("work");
    plain_git(&work, &["update-index", "--skip-worktree", "gate.sh"])?;
    plain_git(&work, &["config", "core.sparseCheckout", "true"])?;

    let outcome = scene.assay_with(&["pre-push"], &[("ASSAY_WEIGHIN", &weighin)])?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (Some(0), ""),
        "a skip-worktree entry and a sparse checkout do not stop the push: {outcome:?}"
    );
    assert!(
        outcome.stdout.starts_with("weigh-in on other: "),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
fn pre_push_refuses_output_weighin_would_not_write() -> Result<(), Box<dyn Error>> {
    let (scene, _) = scene_on("feat-x")?;
    let weighin = scene.path("weighin");
    write_executable(&weighin, "#!/bin/sh\necho '{}'\n")?;

    let outcome = scene.assay_with(&["pre-push"], &[("ASSAY_WEIGHIN", &weighin)])?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            "assay: weighin open did not print a list of items (git push --no-verify skips this \
             check)\n"
        )
    );
    Ok(())
}

#[test]
fn a_blocking_item_fails_pre_push() -> Result<(), Box<dyn Error>> {
    let (scene, weighin) = scene_on("feat-x")?;

    let outcome = scene.assay_with(&["pre-push"], &[("ASSAY_WEIGHIN", &weighin)])?;

    assert_eq!(
        (
            outcome.code,
            outcome.stdout.as_str(),
            outcome.stderr.as_str()
        ),
        (
            Some(1),
            "weigh-in on feat-x: 1 blocking, 1 pending triage\n\
             \x20 blocking wi-7529b9: door one-way, authority\n\
             \x20 pending triage wi-b83949\n\
             \x20 push blocked: settle the blocking items with /weigh-in (git push --no-verify \
             skips this check)\n",
            ""
        )
    );
    Ok(())
}

#[test]
fn a_pending_item_passes_pre_push_with_a_warning() -> Result<(), Box<dyn Error>> {
    let (scene, weighin) = scene_on("other")?;

    let outcome = scene.assay_with(&["pre-push"], &[("ASSAY_WEIGHIN", &weighin)])?;

    assert_eq!(
        (outcome.code, outcome.stdout.as_str()),
        (
            Some(0),
            "weigh-in on other: 0 blocking, 1 pending triage\n\
             \x20 pending triage wi-216116\n\
             \x20 warning: triage the pending items with /weigh-in before the push\n"
        )
    );
    Ok(())
}

#[test]
fn pre_push_takes_no_arguments() -> Result<(), Box<dyn Error>> {
    let (scene, weighin) = scene_on("feat-x")?;

    let outcome = scene.assay_with(&["pre-push", "origin"], &[("ASSAY_WEIGHIN", &weighin)])?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            "assay: usage: assay pre-push, run from the repository root (plugin/hooks/pre-push \
             runs it)\n"
        )
    );
    Ok(())
}

/// `git push <args>` from the scene's working tree to a bare `origin`, with the shipped hooks,
/// `ASSAY` naming this build and `ASSAY_WEIGHIN` the stand-in.
fn push(scene: &Scene, weighin: &Path, args: &[&str]) -> Result<Output, Box<dyn Error>> {
    let mut git = Command::new("git");
    git.env_clear();
    if let Some(path) = env::var_os("PATH") {
        git.env("PATH", path);
    }
    let hooks_path: OsString = ["core.hooksPath=", &hooks().display().to_string()]
        .concat()
        .into();
    Ok(git
        .current_dir(scene.path("work"))
        .arg("-c")
        .arg(hooks_path)
        .arg("push")
        .args(args)
        .env("HOME", scene.path("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ASSAY", env!("CARGO_BIN_EXE_assay"))
        .env("ASSAY_WEIGHIN", weighin)
        .output()?)
}

/// A bare repository at `remote`, added to the scene as `origin`.
fn with_remote(scene: &Scene) -> Result<PathBuf, Box<dyn Error>> {
    let remote = scene.path("remote.git");
    plain_git(
        &scene.path("home"),
        &["init", "--quiet", "--bare", &remote.display().to_string()],
    )?;
    plain_git(
        &scene.path("work"),
        &["remote", "add", "origin", &remote.display().to_string()],
    )?;
    Ok(remote)
}

/// What the push showed the developer: git sends a hook's stdout to its own stderr.
fn shown(output: &Output) -> Result<String, Box<dyn Error>> {
    Ok([
        std::str::from_utf8(&output.stdout)?,
        std::str::from_utf8(&output.stderr)?,
    ]
    .concat())
}

fn branches(remote: &Path) -> Result<String, Box<dyn Error>> {
    plain_git(remote, &["branch", "--format=%(refname:short)"])
}

#[test]
fn the_hook_blocks_a_push_with_a_blocking_item_and_no_verify_bypasses_it()
-> Result<(), Box<dyn Error>> {
    let (scene, weighin) = scene_on("feat-x")?;
    let remote = with_remote(&scene)?;

    let blocked = push(&scene, &weighin, &["--quiet", "origin", "feat-x"])?;
    let after_blocked = branches(&remote)?;
    let bypassed = push(
        &scene,
        &weighin,
        &["--quiet", "--no-verify", "origin", "feat-x"],
    )?;

    let blocked_out = shown(&blocked)?;
    assert!(!blocked.status.success(), "{blocked:?}");
    assert!(
        blocked_out.contains("  blocking wi-7529b9: door one-way, authority\n"),
        "{blocked_out}"
    );
    assert_eq!(after_blocked, "", "the blocked push wrote nothing");
    assert!(bypassed.status.success(), "{bypassed:?}");
    assert_eq!(branches(&remote)?, "feat-x\n");
    Ok(())
}

#[test]
fn the_hook_lets_a_push_with_only_an_untriaged_item_through_with_a_warning()
-> Result<(), Box<dyn Error>> {
    let (scene, weighin) = scene_on("other")?;
    let remote = with_remote(&scene)?;

    let pushed = push(&scene, &weighin, &["--quiet", "origin", "other"])?;

    let out = shown(&pushed)?;
    assert!(pushed.status.success(), "{pushed:?}");
    assert!(
        out.contains("  warning: triage the pending items with /weigh-in before the push\n"),
        "{out}"
    );
    assert_eq!(branches(&remote)?, "other\n");
    Ok(())
}
