//! The release workflow and the composite actions follow ADR-14 and `05` T14: releases only
//! from `v*` tags, `packages: write` only on the publish job, and every third-party action
//! pinned by full commit SHA.

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

/// The jobs (two-space keys under `jobs:`) whose block contains `grant` as a line.
fn jobs_granting<'a>(workflow: &'a str, grant: &str) -> Vec<&'a str> {
    let mut jobs = Vec::new();
    let mut current = None;
    for line in top_level_block(workflow, "jobs") {
        if let Some(job) = line
            .strip_prefix("  ")
            .filter(|rest| !rest.starts_with(' '))
            .and_then(|rest| rest.strip_suffix(':'))
        {
            current = Some(job);
        } else if line.trim() == grant
            && let Some(job) = current
            && !jobs.contains(&job)
        {
            jobs.push(job);
        }
    }
    jobs
}

#[test]
fn only_the_publish_job_may_write_packages() -> Result<(), io::Error> {
    let workflow = release_workflow()?;

    let writers = jobs_granting(&workflow, "packages: write");

    assert_eq!(writers, ["publish"], "jobs granted `packages: write`");
    Ok(())
}

#[test]
fn only_the_release_notes_job_may_write_contents() -> Result<(), io::Error> {
    let workflow = release_workflow()?;

    let writers = jobs_granting(&workflow, "contents: write");

    assert_eq!(writers, ["release-notes"], "jobs granted `contents: write`");
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
