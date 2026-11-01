//! What the conformance tests of both consumer workflow templates share (#60 ST-15, #61 DO-13):
//! `workflows/assay.yml.tmpl` (`pipeline_template.rs`) and `workflows/assay-decide.yml.tmpl`
//! (`decide_only_template.rs`). Built on `workflow.rs`'s line readers.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use super::pipeline_argv::Invocation;
use super::release_workflow::is_pinned;
use super::workflow::{Step, action_files, job_body, job_value, steps, uses};

/// The actions a template may use, by `owner/repo[/path]`: none touches the Actions cache
/// (ADR-21, 05 T6; `workflow_cache.rs` holds the same rule for `.github/`).
pub(crate) const ACTIONS: [&str; 5] = [
    "actions/checkout",
    "actions/upload-artifact",
    "actions/download-artifact",
    "actions/create-github-app-token",
    "Wet-Ink-Corporation/assay/.github/actions/fetch-by-digest",
];

/// The inputs of the step fetching the assay binary (02 §4, 05 T14): the pin from base config,
/// never the PR head, from assay's own package, of the binary's artifact type.
pub(crate) const ASSAY_FETCH: [(&str, &str); 4] = [
    ("repository", "ghcr.io/wet-ink-corporation/assay"),
    ("config", "base/.assay/plugins.toml"),
    ("artifact-type", "application/vnd.wet-ink.assay.binary.v1"),
    ("out-dir", "bin"),
];

/// The `&&` terms of a job's `if`.
pub(crate) fn terms(condition: &str) -> Vec<&str> {
    condition.split(" && ").map(str::trim).collect()
}

/// A job's `needs`, from its one-line flow list.
pub(crate) fn needs<'a>(body: &[&'a str]) -> Vec<&'a str> {
    job_value(body, "needs")
        .and_then(|list| list.strip_prefix('['))
        .and_then(|list| list.strip_suffix(']'))
        .map_or_else(Vec::new, |list| list.split(", ").collect())
}

/// Every step of the template as `(job, step)`.
pub(crate) fn all_steps<'a>(jobs: &'a [(&'a str, Vec<&'a str>)]) -> Vec<(&'a str, Step<'a>)> {
    jobs.iter()
        .flat_map(|(job, body)| steps(body).into_iter().map(move |step| (*job, step)))
        .collect()
}

/// The step of `job` that runs `invocation`.
pub(crate) fn assay_step<'a>(
    jobs: &'a [(&'a str, Vec<&'a str>)],
    invocation: &Invocation,
) -> Option<Step<'a>> {
    steps(job_body(jobs, invocation.job))
        .into_iter()
        .find(|step| step.run().as_deref() == Some(invocation.run_line().as_str()))
}

/// Every raw line of `text` that GitHub reads as a step's `run:` text, as `(line number, line)`:
/// the `run:` key's own line and, whatever its header (`|`, `>+`, `| # note` or a one-line
/// value), every following line up to the first one indented no deeper than the key (comment
/// and blank lines included, since GitHub expands an expression on any of them). Only `run:`
/// keys inside a `steps:` block count, so a job output named `run` is not one. Unlike
/// [`jobs`](super::workflow::jobs), it drops nothing.
pub(crate) fn raw_run_lines(text: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut block: Option<usize> = None;
    let mut steps: Option<usize> = None;
    for (number, line) in (1..).zip(text.lines()) {
        let indent = line.len().saturating_sub(line.trim_start().len());
        if let Some(key_indent) = block {
            if line.trim().is_empty() || indent > key_indent {
                lines.push((number, line));
                continue;
            }
            block = None;
        }
        if line.trim().is_empty() {
            continue;
        }
        if steps.is_some_and(|steps| indent <= steps) {
            steps = None;
        }
        if line.trim() == "steps:" {
            steps = Some(indent);
            continue;
        }
        if steps.is_none() {
            continue;
        }
        let dash = line.trim_start().strip_prefix("- ");
        let key_indent = indent.saturating_add(if dash.is_some() { 2 } else { 0 });
        let key = dash.unwrap_or_else(|| line.trim_start());
        if key.starts_with("run:") {
            // Whatever the header (`|`, `>+`, `| # note`, or a one-line value), every line
            // indented deeper than the key belongs to the value, so all of them are read.
            lines.push((number, line));
            block = Some(key_indent);
        }
    }
    lines
}

/// Each App token minted, by `(job, step id)`: its whole `with:` entry list, in file order.
pub(crate) type Minted<'a> = BTreeMap<(&'a str, &'a str), Vec<(&'a str, &'a str)>>;

/// Every `create-github-app-token` step of `jobs`.
pub(crate) fn app_tokens<'a>(jobs: &'a [(&'a str, Vec<&'a str>)]) -> Minted<'a> {
    all_steps(jobs)
        .into_iter()
        .filter(|(_, step)| {
            step.value("uses")
                .is_some_and(|action| action.starts_with("actions/create-github-app-token@"))
        })
        .map(|(job, step)| {
            (
                (job, step.value("id").unwrap_or_default()),
                step.entries("with"),
            )
        })
        .collect()
}

/// The `with:` list of an `assay-verdict` App token narrowed to the consumer repository and
/// `scopes` (05 §4). No other input is allowed, so neither a wider `owner` nor a missing
/// `repositories` passes.
pub(crate) fn verdict_token(
    scopes: &[(&'static str, &'static str)],
) -> Vec<(&'static str, &'static str)> {
    let mut with = vec![
        ("app-id", "${{ vars.ASSAY_VERDICT_APP_ID }}"),
        ("private-key", "${{ secrets.ASSAY_VERDICT_PRIVATE_KEY }}"),
        ("repositories", "${{ github.event.repository.name }}"),
    ];
    with.extend_from_slice(scopes);
    with
}

/// The `with:` list of an `assay-ledger-writer` token, narrowed to the ledger repository base
/// config names (ADR-26).
pub(crate) fn ledger_token() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app-id", "${{ vars.ASSAY_LEDGER_APP_ID }}"),
        ("private-key", "${{ secrets.ASSAY_LEDGER_PRIVATE_KEY }}"),
        ("owner", "${{ steps.ledger.outputs.owner }}"),
        ("repositories", "${{ steps.ledger.outputs.name }}"),
        ("permission-contents", "write"),
    ]
}

/// The one source of each token, id and upload outcome an assay step reads, in either template.
pub(crate) fn token_variable(name: &str) -> Option<&'static str> {
    Some(match name {
        "ASSAY_RUN_TOKEN" => "${{ github.token }}",
        "ASSAY_GITHUB_TOKEN" => "${{ steps.verdict-token.outputs.token }}",
        "ASSAY_LEDGER_TOKEN" => "${{ steps.ledger-token.outputs.token }}",
        "ASSAY_VERDICT_APP_ID" => "${{ vars.ASSAY_VERDICT_APP_ID }}",
        "ASSAY_VERDICT_BOT_ID" => "${{ vars.ASSAY_VERDICT_BOT_ID }}",
        "ASSAY_PERSIST" => "${{ steps.persist.outcome }}",
        _ => return None,
    })
}

/// A template's `uses:` values, read raw and without their tag comment, and those that break
/// the pinning rules.
#[derive(Debug)]
pub(crate) struct ActionPins<'a> {
    /// Every `uses:` value as written, its `# tag` comment included.
    pub(crate) raw: Vec<&'a str>,
    /// Not pinned by a full commit SHA with a tag comment.
    pub(crate) unpinned: Vec<&'a str>,
    /// Outside [`ACTIONS`].
    pub(crate) unlisted: Vec<&'a str>,
    /// Pinned at another commit than `.github/` pins the same action at.
    pub(crate) other_pins: Vec<&'a str>,
}

/// Reads `workflow`'s `uses:` values against [`ACTIONS`] and the pins of every workflow and
/// composite action under `.github/`.
pub(crate) fn action_pins(workflow: &str) -> io::Result<ActionPins<'_>> {
    let mut repository_pins = BTreeSet::new();
    for file in action_files()? {
        let text = std::fs::read_to_string(file)?;
        repository_pins.extend(uses(text.lines()).into_iter().map(str::to_owned));
    }

    let raw = workflow
        .lines()
        .map(|line| line.trim().trim_start_matches("- "))
        .filter_map(|line| line.strip_prefix("uses:"))
        .map(str::trim)
        .collect::<Vec<_>>();
    let unpinned = raw
        .iter()
        .copied()
        .filter(|value| !is_pinned(value))
        .collect();
    let listed = uses(workflow.lines());
    let unlisted = listed
        .iter()
        .copied()
        .filter(|value| {
            !value
                .split_once('@')
                .is_some_and(|(action, _)| ACTIONS.contains(&action))
        })
        .collect();
    let other_pins = listed
        .iter()
        .copied()
        .filter(|value| {
            let action = value.split_once('@').map_or(*value, |(action, _)| action);
            let known = repository_pins
                .iter()
                .filter(|pin| pin.starts_with(&format!("{action}@")))
                .collect::<Vec<_>>();
            !known.is_empty() && !known.iter().any(|pin| pin == value)
        })
        .collect();
    Ok(ActionPins {
        raw,
        unpinned,
        unlisted,
        other_pins,
    })
}
