//! CP1 (#108, 04 §9) in `assay check --local`, end to end on the fixture repository of
//! `check.rs` with a stand-in `weighin` that prints weighin 0.9.0's recorded
//! `open --repo --json` (the real binary never runs): a blocking item on the branch blocks the
//! push, an untriaged one only warns, output weighin would not write stops the check before
//! anything else runs, and without a `weighin` or a branch CP1 says it did not check.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::check::{Outcome, Scene, plain_git};
use crate::support::write_executable;

/// The terminal report's first line.
const BANNER: &str = "assay check --local: advisory, CI never reads this report\n";

/// weighin 0.9.0's `open --repo --json`: on `feat-x` a triaged one-way authority question
/// (`wi-7529b9`), an untriaged one (`wi-b83949`) and a clear two-way judgment call; on `other`
/// an untriaged one (`wi-216116`).
fn recorded() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assay-app/tests/fixtures/weigh-in/open-0.9.0.json")
}

/// The fixture scene on a new `branch`. Its decision fails whatever CP1 finds: local mode
/// computes no package sets (#65), so every change is exec surface. The exit code CP1 alone
/// gives is pinned by `check.rs`'s unit tests (`passes`).
fn on_branch(branch: &str) -> Result<Scene, Box<dyn Error>> {
    let scene = Scene::new()?;
    plain_git(&scene.path("work"), &["checkout", "--quiet", "-b", branch])?;
    Ok(scene)
}

/// A stand-in `weighin` that accepts only CP1's invocation, records its environment in
/// `weighin-env` and prints `output`.
fn stand_in(scene: &Scene, output: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let program = scene.path("weighin");
    let script = format!(
        "#!/bin/sh\n[ \"$*\" = 'open --repo --json --top 4097' ] || {{ echo \"$*\" >&2; exit 64; }}\n\
         /usr/bin/env > '{env}'\n/bin/cat '{output}'\n",
        env = scene.path("weighin-env").display(),
        output = output.display(),
    );
    write_executable(&program, &script)?;
    Ok(program)
}

fn check(scene: &Scene, weighin: &Path) -> Result<Outcome, Box<dyn Error>> {
    let claude = scene.claude()?;
    scene.check_with(
        &["--local", "--claude", &claude],
        &[("ASSAY_WEIGHIN", weighin)],
    )
}

#[test]
fn a_blocking_item_on_the_branch_blocks_the_push() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("feat-x")?;
    let weighin = stand_in(&scene, &recorded())?;

    let outcome = check(&scene, &weighin)?;

    assert!(outcome.stdout.starts_with(BANNER), "{outcome:?}");
    assert!(
        outcome.stdout.ends_with(
            "weigh-in on feat-x: 1 blocking, 1 pending triage\n\
             \x20 blocking wi-7529b9: door one-way, authority\n\
             \x20 pending triage wi-b83949\n\
             \x20 push blocked: settle the blocking items with /weigh-in (git push --no-verify \
             skips this check)\n"
        ),
        "{outcome:?}"
    );
    assert_eq!(outcome.code, Some(1), "a blocking item fails the check");
    Ok(())
}

#[test]
fn an_untriaged_item_on_the_branch_warns() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("other")?;
    let weighin = stand_in(&scene, &recorded())?;

    let outcome = check(&scene, &weighin)?;

    assert!(
        outcome.stdout.ends_with(
            "weigh-in on other: 0 blocking, 1 pending triage\n\
             \x20 pending triage wi-216116\n\
             \x20 warning: triage the pending items with /weigh-in before the push\n"
        ),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
fn weighin_runs_in_the_tree_with_only_path_home_and_its_store() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("other")?;
    let weighin = stand_in(&scene, &recorded())?;
    let store = scene.path("ledger.db");
    let claude = scene.claude()?;

    let outcome = scene.check_with(
        &["--local", "--claude", &claude],
        &[("ASSAY_WEIGHIN", &weighin), ("WEIGHIN_STORE", &store)],
    )?;

    let env = fs::read_to_string(scene.path("weighin-env"))?;
    // The variables a shell sets for itself are not assay's.
    let shells = ["PWD", "OLDPWD", "SHLVL", "_"];
    let mut names: Vec<&str> = env
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .filter(|name| !shells.contains(name))
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["HOME", "PATH", "WEIGHIN_STORE"], "{outcome:?}");
    assert!(env.contains(&format!("WEIGHIN_STORE={}\n", store.display())));
    assert!(
        env.contains(&format!("PWD={}\n", scene.path("work").display())),
        "{env}"
    );
    Ok(())
}

#[test]
fn output_weighin_would_not_write_stops_the_check_before_the_snapshot() -> Result<(), Box<dyn Error>>
{
    let scene = on_branch("feat-x")?;
    let bad = scene.path("bad.json");
    fs::write(&bad, r#"[{"id":"wi-7529b9","status":"decided"}]"#)?;
    let weighin = stand_in(&scene, &bad)?;

    let outcome = check(&scene, &weighin)?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            "assay: item 0 of weighin open: status is missing or not a value weighin writes \
             (git push --no-verify skips this check)\n"
        ),
        "{outcome:?}"
    );
    assert!(!scene.path("seen-tree").exists(), "the model never ran");
    Ok(())
}

#[test]
fn a_weighin_that_writes_past_the_output_cap_stops_the_check() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("feat-x")?;
    let weighin = scene.path("weighin");
    // One byte over `MAX_OPEN_BYTES` (8 MiB).
    let over = assay_app::MAX_OPEN_BYTES + 1;
    write_executable(
        &weighin,
        &format!("#!/bin/sh\n/usr/bin/head -c {over} /dev/zero\n"),
    )?;

    let outcome = check(&scene, &weighin)?;

    assert_eq!(outcome.code, Some(2), "{outcome:?}");
    assert!(
        outcome
            .stderr
            .starts_with("assay: weighin open did not run: "),
        "{outcome:?}"
    );
    assert!(!scene.path("seen-tree").exists(), "the model never ran");
    Ok(())
}

#[test]
fn a_weighin_that_fails_stops_the_check() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("feat-x")?;
    let weighin = scene.path("weighin");
    write_executable(&weighin, "#!/bin/sh\nexit 3\n")?;

    let outcome = check(&scene, &weighin)?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            "assay: weighin open failed: exit status: 3 (git push --no-verify skips this \
             check)\n"
        ),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
fn a_relative_weighin_program_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("feat-x")?;

    let outcome = check(&scene, Path::new("bin/weighin"))?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            "assay: ASSAY_WEIGHIN names bin/weighin, which is not an absolute path\n"
        )
    );
    Ok(())
}

#[test]
fn without_a_weighin_cp1_says_it_did_not_check() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("feat-x")?;
    // A `PATH` holding only git, so a weighin installed on this machine is not found.
    let bin = scene.path("bin");
    fs::create_dir(&bin)?;
    let git = env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .ok_or("no git on PATH")?;
    std::os::unix::fs::symlink(git, bin.join("git"))?;

    let outcome = scene.run(
        &scene.path("work"),
        Some(bin.into_os_string()),
        &["--local", "--claude", &scene.claude()?],
    )?;

    assert!(
        outcome
            .stdout
            .ends_with("weigh-in: not checked: no weighin on PATH and ASSAY_WEIGHIN is not set\n"),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
fn on_a_detached_head_cp1_says_it_did_not_check() -> Result<(), Box<dyn Error>> {
    let scene = on_branch("feat-x")?;
    plain_git(&scene.path("work"), &["checkout", "--quiet", "--detach"])?;
    let weighin = stand_in(&scene, &recorded())?;

    let outcome = check(&scene, &weighin)?;

    assert!(
        outcome.stdout.ends_with(
            "weigh-in: not checked: HEAD is detached, and items are filtered by branch\n"
        ),
        "{outcome:?}"
    );
    assert!(!scene.path("weighin-env").exists(), "weighin never ran");
    Ok(())
}
