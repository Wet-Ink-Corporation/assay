//! What `assay gate gates-lint` (#60 ST-8, #348) refuses of its `--head` checkout: the job runs
//! PR-controlled code (clippy builds it), so it fetches `ASSAY_HEAD_SHA` into an object-only
//! repository and runs nothing unless the checkout holds exactly that commit's tree. Every
//! difference exits 2 after the fetch, with no step, node or exec gate run and nothing written:
//! a tracked file changed, missing or with another executable bit, a checkout of another commit,
//! and an untracked file. Only the checkout's own `.git` and an untracked `target/` directory
//! (where the warm `target/` is restored, ADR-21) are not the tree's.

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Output;

use crate::gate::temper;
use crate::gate_static_scene::Scene;

/// A gates-lint scene whose only lint step leaves `fmt-ran` in the head tree.
fn scene() -> Result<Scene, Box<dyn Error>> {
    Scene::lint(
        &[("src/lib.rs", "pub fn two() -> u8 {\n    2\n}\n")],
        &temper(&[("fmt", "lint", "touch fmt-ran")]),
    )
}

/// The job exited 2 naming `path` after fetching, and ran and wrote nothing.
fn refused(scene: &Scene, output: &Output, path: &str) -> Result<(), Box<dyn Error>> {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("assay: the head checkout ")
            && stderr.contains(&scene.head_sha)
            && stderr.contains(&format!("`{path}`")),
        "{stderr}"
    );
    assert!(
        !scene.recorded("git-bin/fetches")?.is_empty(),
        "the head was fetched to compare against"
    );
    assert!(!scene.head.join("fmt-ran").exists(), "no step ran");
    assert_eq!(scene.recorded("bin/node-env")?, "", "no temper ran");
    assert!(!scene.path("out").exists(), "nothing was written");
    assert_eq!(scene.recorded("output")?, "", "no output was appended");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_tracked_file_that_differs_from_the_head_commit_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = scene()?;
    fs::write(
        scene.head.join("src/lib.rs"),
        "pub fn two() -> u8 {\n    3\n}\n",
    )?;

    let output = scene.run(|_| {})?;

    refused(&scene, &output, "src/lib.rs")
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_checkout_of_another_commit_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = scene()?;
    scene.in_checkout(&["checkout", "--quiet", &scene.base_sha])?;

    let output = scene.run(|_| {})?;

    refused(&scene, &output, "src/lib.rs")
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_missing_tracked_file_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = scene()?;
    fs::remove_file(scene.head.join(".claude/temper.json"))?;

    let output = scene.run(|_| {})?;

    refused(&scene, &output, ".claude/temper.json")
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_tracked_file_made_executable_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = scene()?;
    let lib = scene.head.join("src/lib.rs");
    fs::set_permissions(&lib, fs::Permissions::from_mode(0o755))?;

    let output = scene.run(|_| {})?;

    refused(&scene, &output, "src/lib.rs")
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn a_tracked_symlink_retargeted_to_a_path_of_the_same_length_is_refused()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::lint_with_links(
        &[],
        &[("docs-link", "src/lib.rs")],
        &temper(&[("fmt", "lint", "touch fmt-ran")]),
    )?;
    let link = scene.head.join("docs-link");
    fs::remove_file(&link)?;
    // `Cargo.tom_` and `src/lib.rs` are both ten bytes: only the target's text tells them apart.
    std::os::unix::fs::symlink("Cargo.tom_", &link)?;

    let output = scene.run(|_| {})?;

    refused(&scene, &output, "docs-link")
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_untracked_file_in_the_checkout_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = scene()?;
    fs::create_dir(scene.head.join(".cargo"))?;
    fs::write(
        scene.head.join(".cargo/config.toml"),
        "[build]\nrustflags = []\n",
    )?;

    let output = scene.run(|_| {})?;

    refused(&scene, &output, ".cargo/config.toml")
}

#[test]
#[cfg_attr(miri, ignore = "spawns the `assay` binary; Miri cannot run processes")]
fn an_untracked_target_directory_is_not_the_checkouts_content() -> Result<(), Box<dyn Error>> {
    let scene = scene()?;
    fs::create_dir_all(scene.head.join("target/debug"))?;
    fs::write(scene.head.join("target/debug/fixture.d"), "restored\n")?;

    let output = scene.run(|_| {})?;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "temper pass\n");
    Ok(())
}
