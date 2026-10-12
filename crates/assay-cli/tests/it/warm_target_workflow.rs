//! `.github/workflows/warm-target.yml` and the restore action
//! `.github/actions/warm-target/action.yml` (ADR-21, `06 §3`, `05` T6): the warm `target/` is
//! built only by a trusted job on pushes to `main`, at the fixed path Z2b builds at (A-29), with
//! the workspace crates cleaned out; only `publish` may write packages; and the restore cleans
//! the workspace crates after unpacking, before anything builds.

use std::collections::BTreeMap;
use std::fs;
use std::io;

use super::workflow::{job_body, jobs, permissions, repository_root, top_level_block, workflow};

fn warm_target_workflow() -> io::Result<String> {
    workflow("warm-target.yml")
}

/// The index of the first line in `lines` containing `needle`.
fn position(lines: &[&str], needle: &str) -> Option<usize> {
    lines.iter().position(|line| line.contains(needle))
}

#[test]
fn warm_target_runs_only_for_pushes_to_main() -> Result<(), io::Error> {
    let workflow = warm_target_workflow()?;

    let trigger = top_level_block(&workflow, "on");

    assert_eq!(
        trigger,
        ["on:", "  push:", "    branches: [main]"],
        "warm-target.yml must trigger on pushes to main and nothing else"
    );
    Ok(())
}

#[test]
fn workflow_default_permission_is_contents_read() -> Result<(), io::Error> {
    let workflow = warm_target_workflow()?;

    let permissions = top_level_block(&workflow, "permissions");

    assert_eq!(
        permissions,
        ["permissions:", "  contents: read"],
        "warm-target.yml must default every job to `contents: read`"
    );
    Ok(())
}

#[test]
fn each_job_is_granted_exactly_its_permissions() -> Result<(), io::Error> {
    let workflow = warm_target_workflow()?;

    let granted = jobs(&workflow)
        .iter()
        .map(|(job, body)| ((*job).to_owned(), permissions(body)))
        .collect::<BTreeMap<_, _>>();

    let grants = |scopes: &[&str]| Some(scopes.iter().map(|&s| s.to_owned()).collect());
    let expected = BTreeMap::from([
        ("build".to_owned(), None),
        ("publish".to_owned(), grants(&["packages: write"])),
        (
            "verify".to_owned(),
            grants(&["contents: read", "packages: read"]),
        ),
    ]);
    assert_eq!(granted, expected, "per-job permissions of warm-target.yml");
    Ok(())
}

#[test]
fn build_runs_only_for_the_main_branch_ref() -> Result<(), io::Error> {
    let workflow = warm_target_workflow()?;
    let jobs = jobs(&workflow);

    let build = job_body(&jobs, "build");

    assert!(
        build.contains(&"    if: github.ref == 'refs/heads/main'"),
        "the build job must refuse any ref but main: {build:?}"
    );
    Ok(())
}

#[test]
fn build_compiles_at_the_fixed_path_and_cleans_workspace_crates_before_packing()
-> Result<(), io::Error> {
    let workflow = warm_target_workflow()?;
    let jobs = jobs(&workflow);
    let build = job_body(&jobs, "build");

    let env = top_level_block(&workflow, "env");
    let steps = [
        position(build, r#"cp -a -- "$GITHUB_WORKSPACE/." "$WORKSPACE/""#),
        position(build, "working-directory: ${{ env.WORKSPACE }}"),
        position(build, "cargo clippy --locked"),
        position(build, "cargo test --locked"),
        position(build, r#""$SCRIPT" clean "$WORKSPACE""#),
        position(build, r#""$SCRIPT" pack "$WORKSPACE""#),
    ];

    assert!(
        env.contains(&"  WORKSPACE: /work/ws"),
        "the workspace path must be fixed (A-29): {env:?}"
    );
    assert!(
        steps.iter().all(Option::is_some) && steps.is_sorted(),
        "place, build at WORKSPACE, clean, then pack: {steps:?}"
    );
    Ok(())
}

#[test]
fn publish_runs_no_repository_code() -> Result<(), io::Error> {
    let workflow = warm_target_workflow()?;
    let jobs = jobs(&workflow);
    let publish = job_body(&jobs, "publish");

    let repository_code = publish
        .iter()
        .map(|line| line.trim())
        .filter(|line| {
            line.contains("actions/checkout") || line.contains("cargo") || line.contains("./")
        })
        .collect::<Vec<_>>();

    assert!(!publish.is_empty(), "warm-target.yml has no publish job");
    assert_eq!(
        repository_code,
        Vec::<&str>::new(),
        "publish holds packages: write"
    );
    Ok(())
}

#[test]
fn restore_verifies_then_unpacks_then_cleans_workspace_crates() -> Result<(), io::Error> {
    let action =
        fs::read_to_string(repository_root().join(".github/actions/warm-target/action.yml"))?;
    let lines = action.lines().collect::<Vec<_>>();

    let steps = [
        position(&lines, r#"bash "$FETCH" fetch "$REPOSITORY" "$DIGEST""#),
        position(&lines, r#"bash "$SCRIPT" unpack"#),
        position(&lines, r#"bash "$SCRIPT" clean "$WORKSPACE""#),
    ];

    assert!(
        steps.iter().all(Option::is_some) && steps.is_sorted(),
        "fetch by digest, unpack, then cargo clean: {steps:?}"
    );
    Ok(())
}
