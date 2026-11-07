//! Line-level readers for the workflow and composite-action YAML under `.github/` and the
//! workflow templates under `workflows/`, shared by the workflow tests. They read the block
//! layout the files are written in (two-space indents, one key per line), not YAML in general;
//! a file that strays from it reads as missing pieces, which the tests then report.

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

/// The text of the consumer workflow template `workflows/<name>` (02 §7).
pub(crate) fn template(name: &str) -> io::Result<String> {
    fs::read_to_string(repository_root().join("workflows").join(name))
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

/// The value of a job's one-line key `key:` (`if`, `needs`, `timeout-minutes`).
pub(crate) fn job_value<'a>(body: &[&'a str], key: &str) -> Option<&'a str> {
    let prefix = format!("    {key}: ");
    body.iter()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
}

/// The entries of a job's mapping `key:` (`outputs`, `concurrency`), trimmed, in file order;
/// empty when the job has none.
pub(crate) fn job_entries<'a>(body: &[&'a str], key: &str) -> Vec<&'a str> {
    let header = format!("    {key}:");
    body.iter()
        .skip_while(|line| **line != header)
        .skip(1)
        .take_while(|line| line.starts_with("      "))
        .map(|line| line.trim())
        .collect()
}

/// One step of a job: its first line without the `      - ` that opens it, and each later line
/// without the step's own eight-space indent, so the step's keys start a line and their
/// children are indented by two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step<'a> {
    lines: Vec<&'a str>,
}

impl<'a> Step<'a> {
    /// The value of the step's one-line key `key:`.
    pub(crate) fn value(&self, key: &str) -> Option<&'a str> {
        let prefix = format!("{key}: ");
        self.lines
            .iter()
            .find_map(|line| line.strip_prefix(prefix.as_str()))
    }

    /// The entries of the step's mapping `key:` (`with`, `env`) as `(name, value)`.
    pub(crate) fn entries(&self, key: &str) -> Vec<(&'a str, &'a str)> {
        let header = format!("{key}:");
        self.lines
            .iter()
            .skip_while(|line| **line != header)
            .skip(1)
            .take_while(|line| line.starts_with("  "))
            .filter_map(|line| line.trim().split_once(": "))
            .collect()
    }

    /// The value of the step's mapping entry `key.name`.
    pub(crate) fn entry(&self, key: &str, name: &str) -> Option<&'a str> {
        self.entries(key)
            .into_iter()
            .find_map(|(entry, value)| (entry == name).then_some(value))
    }

    /// The step's `run:` text: its one-line value, or the lines of its `|` block joined by
    /// newlines (comment and blank lines are already dropped by [`jobs`]).
    pub(crate) fn run(&self) -> Option<String> {
        let value = self.value("run")?;
        if value != "|" {
            return Some(value.to_owned());
        }
        let block = self
            .lines
            .iter()
            .skip_while(|line| **line != "run: |")
            .skip(1)
            .take_while(|line| line.starts_with("  "))
            .map(|line| line.get(2..).unwrap_or_default())
            .collect::<Vec<_>>();
        Some(block.join("\n"))
    }
}

/// The steps of a job body, in file order. A line indented less than a step's ends the
/// `steps:` block.
pub(crate) fn steps<'a>(body: &[&'a str]) -> Vec<Step<'a>> {
    let mut steps: Vec<Step<'a>> = Vec::new();
    let mut in_steps = false;
    for &line in body {
        if !line.starts_with("      ") {
            in_steps = line == "    steps:";
            continue;
        }
        if !in_steps {
            continue;
        }
        if let Some(first) = line.strip_prefix("      - ") {
            steps.push(Step { lines: vec![first] });
        } else if let (Some(step), Some(rest)) = (steps.last_mut(), line.strip_prefix("        ")) {
            step.lines.push(rest);
        }
    }
    steps
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
    action_files_in(&repository_root().join(".github"))
}

/// [`action_files`] under `github`. Every entry of `actions/` must be a directory holding an
/// `action.yml` and no `action.yaml`, which GitHub would also run but no scan would read.
pub(crate) fn action_files_in(github: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(github.join("workflows"))? {
        files.push(entry?.path());
    }
    for entry in fs::read_dir(github.join("actions"))? {
        let dir = entry?.path();
        let action = dir.join("action.yml");
        if !action.is_file() || dir.join("action.yaml").exists() {
            return Err(io::Error::other(format!(
                "{} must hold action.yml and no action.yaml",
                dir.display()
            )));
        }
        files.push(action);
    }
    files.sort();
    Ok(files)
}

/// Whether `name` is one plain path segment naming a composite action of this repository,
/// `.github/actions/<name>/action.yml`.
pub(crate) fn is_local_action(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && repository_root()
            .join(".github/actions")
            .join(name)
            .join("action.yml")
            .is_file()
}
