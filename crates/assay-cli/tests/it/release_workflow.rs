//! The release workflow and the composite actions follow ADR-14 and `05` T14: releases only
//! from `v*` tags, `packages: write` only on the publish job, and every third-party action
//! pinned by full commit SHA.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn release_workflow() -> io::Result<String> {
    fs::read_to_string(repository_root().join(".github/workflows/release.yml"))
}

/// The lines of the top-level mapping `key:`, from its header to the next top-level key.
fn top_level_block<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    let header = format!("{key}:");
    text.lines()
        .skip_while(|line| *line != header)
        .enumerate()
        .take_while(|&(index, line)| {
            index == 0 || line.is_empty() || line.starts_with(' ') || line.starts_with('#')
        })
        .map(|(_, line)| line)
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect()
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

/// Each job under `jobs:` with the lines of its body, comments and blank lines dropped.
fn jobs(workflow: &str) -> Vec<(&str, Vec<&str>)> {
    let mut jobs: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in top_level_block(workflow, "jobs").into_iter().skip(1) {
        if let Some(job) = line
            .strip_prefix("  ")
            .filter(|rest| !rest.starts_with(' '))
            .and_then(|rest| rest.strip_suffix(':'))
        {
            jobs.push((job, Vec::new()));
        } else if let Some((_, body)) = jobs.last_mut() {
            body.push(line);
        }
    }
    jobs
}

/// A job's `permissions:` entries as `scope: level`; `None` when it inherits the workflow's.
/// An inline value (`permissions: write-all`, `permissions: {...}`) is one `inline:` entry.
fn permissions(body: &[&str]) -> Option<Vec<String>> {
    let mut lines = body
        .iter()
        .skip_while(|line| !line.starts_with("    permissions:"));
    let header = lines.next()?;
    let inline = header.trim_start_matches("    permissions:").trim();
    if !inline.is_empty() {
        return Some(vec![format!("inline: {inline}")]);
    }
    Some(
        lines
            .take_while(|line| line.starts_with("      "))
            .map(|line| line.trim().to_owned())
            .collect(),
    )
}

fn job_body<'a>(jobs: &'a [(&str, Vec<&'a str>)], name: &str) -> &'a [&'a str] {
    jobs.iter()
        .find(|(job, _)| *job == name)
        .map_or(&[], |(_, body)| body.as_slice())
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

/// Workflows and composite actions: every YAML file under `.github/workflows` and
/// `.github/actions/*/action.yml`.
fn action_files() -> io::Result<Vec<PathBuf>> {
    let github = repository_root().join(".github");
    let mut files = Vec::new();
    for entry in fs::read_dir(github.join("workflows"))? {
        files.push(entry?.path());
    }
    for entry in fs::read_dir(github.join("actions"))? {
        let action = entry?.path().join("action.yml");
        if action.is_file() {
            files.push(action);
        }
    }
    files.sort();
    Ok(files)
}

/// Whether a `uses:` value is a local action or `owner/repo[/path]@<40 hex> # <tag>`.
fn is_pinned(uses: &str) -> bool {
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
