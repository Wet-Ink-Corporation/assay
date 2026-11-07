//! The release workflow and the composite actions follow ADR-14 and `05` T14: releases only
//! from `v*` tags, `packages: write` only on the publish job, and every third-party action
//! pinned by full commit SHA.

use std::collections::BTreeMap;
use std::fs;
use std::io;

use super::workflow::{action_files, job_body, jobs, permissions, top_level_block, workflow};

fn release_workflow() -> io::Result<String> {
    workflow("release.yml")
}

#[test]
fn release_runs_only_for_pushed_v_tags() -> Result<(), io::Error> {
    let workflow = release_workflow()?;

    let trigger = top_level_block(&workflow, "on");

    assert_eq!(
        trigger,
        ["on:", "  push:", "    tags: [\"v*\"]"],
        "release.yml must trigger on `v*` tag pushes and nothing else"
    );
    Ok(())
}

#[test]
fn workflow_default_permission_is_contents_read() -> Result<(), io::Error> {
    let workflow = release_workflow()?;

    let permissions = top_level_block(&workflow, "permissions");

    assert_eq!(
        permissions,
        ["permissions:", "  contents: read"],
        "release.yml must default every job to `contents: read`"
    );
    Ok(())
}

#[test]
fn each_job_is_granted_exactly_its_permissions() -> Result<(), io::Error> {
    let workflow = release_workflow()?;

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
        ("release-notes".to_owned(), grants(&["contents: write"])),
    ]);
    assert_eq!(granted, expected, "per-job permissions of release.yml");
    Ok(())
}

#[test]
fn no_permissions_are_granted_in_bulk_or_in_flow_style() -> Result<(), io::Error> {
    let workflow = release_workflow()?;

    let bulk = workflow
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| {
            line.contains("write-all")
                || line.contains("read-all")
                || line
                    .strip_prefix("permissions:")
                    .is_some_and(|value| !value.trim().is_empty())
        })
        .collect::<Vec<_>>();

    assert_eq!(bulk, Vec::<&str>::new(), "bulk or flow-style permissions");
    Ok(())
}

#[test]
fn release_notes_job_runs_no_action() -> Result<(), io::Error> {
    let workflow = release_workflow()?;
    let jobs = jobs(&workflow);
    let body = job_body(&jobs, "release-notes");

    let actions = body
        .iter()
        .map(|line| line.trim().trim_start_matches("- "))
        .filter(|line| line.starts_with("uses:"))
        .collect::<Vec<_>>();

    assert!(!body.is_empty(), "release.yml has no release-notes job");
    assert_eq!(
        actions,
        Vec::<&str>::new(),
        "release-notes holds contents: write"
    );
    Ok(())
}

#[test]
fn build_checks_the_tag_is_on_main_before_building() -> Result<(), io::Error> {
    let workflow = release_workflow()?;
    let jobs = jobs(&workflow);
    let build = job_body(&jobs, "build");

    let ancestry = build.iter().position(|line| {
        line.contains(r#"git merge-base --is-ancestor "$GITHUB_SHA" origin/main"#)
    });
    let compile = build.iter().position(|line| line.contains("cargo build"));

    assert!(
        matches!((ancestry, compile), (Some(check), Some(build)) if check < build),
        "the ancestry check must precede `cargo build`: {ancestry:?}, {compile:?}"
    );
    Ok(())
}

/// Whether a `uses:` value is a local action or `owner/repo[/path]@<40 hex> # <tag>`.
pub(crate) fn is_pinned(uses: &str) -> bool {
    if uses.starts_with("./") {
        return true;
    }
    let Some((_, pin)) = uses.split_once('@') else {
        return false;
    };
    let Some((sha, comment)) = pin.split_once(" # ") else {
        return false;
    };
    sha.len() == 40
        && sha
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && !comment.trim().is_empty()
}

#[test]
fn every_action_is_pinned_by_full_commit_sha_with_a_tag_comment() -> Result<(), io::Error> {
    let files = action_files()?;
    let mut unpinned = Vec::new();

    for file in &files {
        for line in fs::read_to_string(file)?.lines() {
            let trimmed = line.trim_start().trim_start_matches("- ");
            if let Some(uses) = trimmed.strip_prefix("uses:")
                && !is_pinned(uses.trim())
            {
                unpinned.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }

    assert!(
        files.iter().any(|file| file.ends_with("release.yml")),
        "release.yml was not scanned"
    );
    assert_eq!(unpinned, Vec::<String>::new(), "actions not pinned by SHA");
    Ok(())
}
