//! Line-level readers for the workflow and composite-action YAML under `.github/`, shared by
//! the workflow tests. They read the block layout the files are written in (two-space indents,
//! one key per line), not YAML in general; a file that strays from it reads as missing pieces,
//! which the tests then report.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The text of `.github/workflows/<name>`.
pub(crate) fn workflow(name: &str) -> io::Result<String> {
    fs::read_to_string(repository_root().join(".github/workflows").join(name))
}

/// The lines of the top-level mapping `key:`, from its header to the next top-level key.
pub(crate) fn top_level_block<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
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

/// Each job under `jobs:` with the lines of its body, comments and blank lines dropped.
pub(crate) fn jobs(workflow: &str) -> Vec<(&str, Vec<&str>)> {
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
pub(crate) fn permissions(body: &[&str]) -> Option<Vec<String>> {
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

pub(crate) fn job_body<'a>(jobs: &'a [(&str, Vec<&'a str>)], name: &str) -> &'a [&'a str] {
    jobs.iter()
        .find(|(job, _)| *job == name)
        .map_or(&[], |(_, body)| body.as_slice())
}

/// The value of every `uses:` line in `lines`, without its trailing `# tag` comment.
pub(crate) fn uses<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    lines
        .into_iter()
        .map(|line| line.trim().trim_start_matches("- "))
        .filter_map(|line| line.strip_prefix("uses:"))
        .map(|value| {
            value
                .split_once(" #")
                .map_or(value, |(uses, _)| uses)
                .trim()
        })
        .collect()
}

/// Workflows and composite actions: every YAML file under `.github/workflows` and
/// `.github/actions/*/action.yml`.
pub(crate) fn action_files() -> io::Result<Vec<PathBuf>> {
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
