//! ADR-21 and `05` T6: no decision-bearing job uses the Actions cache, because it can be
//! pre-seeded under the base scope with a run's runtime token and a poisoned entry outlives the
//! merge. Both lists below are allowlists (wi-b5affa): a job is decision-bearing unless it is
//! exempted by name, and such a job, like every composite action, may use only actions known not
//! to touch the cache. A new action is therefore reviewed for caching before it can run there.

use std::io;
use std::path::Path;

use super::workflow::{action_files, jobs, uses};

/// Jobs no assay decision reads. `rust.yml` is the repository's own developer feedback; assay's
/// Z2b re-runs the gates from base configuration because a PR can edit it (`06 §3`).
const CACHE_EXEMPT_JOBS: &[(&str, &str)] = &[
    ("rust.yml", "gate"),
    ("rust.yml", "aarch64"),
    ("rust.yml", "miri"),
];

/// Actions, by `owner/repo`, that neither restore nor save the Actions cache.
const CACHE_FREE_ACTIONS: &[&str] = &[
    "actions/checkout",
    "actions/upload-artifact",
    "actions/download-artifact",
    "dtolnay/rust-toolchain",
    "oras-project/setup-oras",
];

/// Whether `uses` is an allowlisted action at some ref, or a composite action of this
/// repository (which [`composite_actions_use_only_cache_free_actions`] scans in turn).
fn is_cache_free(uses: &str) -> bool {
    if let Some(local) = uses.strip_prefix("./.github/actions/") {
        return !local.is_empty() && !local.contains("..");
    }
    uses.split_once('@')
        .is_some_and(|(action, _)| CACHE_FREE_ACTIONS.contains(&action))
}

/// What in `lines` may reach the cache: each `uses:` that is not cache-free, and each line
/// that mentions `uses:` without being a plain `uses:` line (a flow-style step the reader
/// cannot judge).
fn cache_risks<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let unreadable = lines
        .iter()
        .copied()
        .filter(|line| line.contains("uses:"))
        .filter(|line| uses([*line]).is_empty());
    let unlisted = lines
        .iter()
        .copied()
        .filter(|line| uses([*line]).iter().any(|value| !is_cache_free(value)));
    unreadable.chain(unlisted).map(str::trim).collect()
}

/// `file/job: line` for every cache risk in the decision-bearing jobs of `workflow`.
fn decision_job_risks(file: &str, workflow: &str) -> Vec<String> {
    jobs(workflow)
        .into_iter()
        .filter(|(job, _)| !CACHE_EXEMPT_JOBS.contains(&(file, job)))
        .flat_map(|(job, body)| {
            cache_risks(&body)
                .into_iter()
                .map(move |line| format!("{file}/{job}: {line}"))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn file_name(path: &Path) -> io::Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other(format!("no UTF-8 file name: {}", path.display())))
}

/// The workflow files under `.github/workflows`, as `(file name, text)`.
fn workflows() -> io::Result<Vec<(String, String)>> {
    action_files()?
        .iter()
        .filter(|path| path.parent().is_some_and(|dir| dir.ends_with("workflows")))
        .map(|path| Ok((file_name(path)?.to_owned(), std::fs::read_to_string(path)?)))
        .collect()
}

const SYNTHETIC: &str = "\
jobs:
  gates-lint:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: actions/cache@0400d5f644dc74513175e3cd8d07132dd4860809 # v4.2.4
      - uses: actions/cache/restore@0400d5f644dc74513175e3cd8d07132dd4860809 # v4.2.4
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - { name: inline, uses: actions/cache@0400d5f644dc74513175e3cd8d07132dd4860809 }
      - uses: ./.github/actions/fetch-by-digest
";

#[test]
fn finds_every_cache_step_in_a_decision_bearing_job() {
    let risks = decision_job_risks("assay.yml", SYNTHETIC);

    assert_eq!(
        risks,
        [
            "assay.yml/gates-lint: - { name: inline, uses: actions/cache@0400d5f644dc74513175e3cd8d07132dd4860809 }",
            "assay.yml/gates-lint: - uses: actions/cache@0400d5f644dc74513175e3cd8d07132dd4860809 # v4.2.4",
            "assay.yml/gates-lint: - uses: actions/cache/restore@0400d5f644dc74513175e3cd8d07132dd4860809 # v4.2.4",
            "assay.yml/gates-lint: - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2",
        ]
    );
}

#[test]
fn an_exempt_job_is_not_held_to_the_rule() {
    let exempt = SYNTHETIC.replace("gates-lint", "gate");

    let risks = decision_job_risks("rust.yml", &exempt);

    assert_eq!(risks, Vec::<String>::new(), "rust.yml/gate is exempt");
}

#[test]
fn no_decision_bearing_job_uses_an_action_outside_the_cache_free_allowlist() -> Result<(), io::Error>
{
    let workflows = workflows()?;

    let without_jobs = workflows
        .iter()
        .filter(|(_, text)| jobs(text).is_empty())
        .map(|(file, _)| file.as_str())
        .collect::<Vec<_>>();
    let risks = workflows
        .iter()
        .flat_map(|(file, text)| decision_job_risks(file, text))
        .collect::<Vec<_>>();

    assert!(
        workflows.iter().any(|(file, _)| file == "warm-target.yml"),
        "warm-target.yml was not scanned"
    );
    assert_eq!(
        without_jobs,
        Vec::<&str>::new(),
        "workflows read as jobless"
    );
    assert_eq!(risks, Vec::<String>::new(), "cache risks in decision jobs");
    Ok(())
}

#[test]
fn composite_actions_use_only_cache_free_actions() -> Result<(), io::Error> {
    let actions = action_files()?
        .into_iter()
        .filter(|path| path.ends_with("action.yml"))
        .collect::<Vec<_>>();

    let mut risks = Vec::new();
    for action in &actions {
        let text = std::fs::read_to_string(action)?;
        let lines = text.lines().collect::<Vec<_>>();
        risks.extend(
            cache_risks(&lines)
                .into_iter()
                .map(|line| format!("{}: {line}", action.display())),
        );
    }

    assert!(
        actions
            .iter()
            .any(|path| path.ends_with("warm-target/action.yml")),
        "the warm-target action was not scanned"
    );
    assert_eq!(
        risks,
        Vec::<String>::new(),
        "cache risks in composite actions"
    );
    Ok(())
}

#[test]
fn every_cache_exempt_job_exists() -> Result<(), io::Error> {
    let workflows = workflows()?;

    let stale = CACHE_EXEMPT_JOBS
        .iter()
        .filter(|(file, job)| {
            !workflows
                .iter()
                .any(|(name, text)| name == file && jobs(text).iter().any(|(j, _)| j == job))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        stale,
        Vec::<&(&str, &str)>::new(),
        "exemptions for missing jobs"
    );
    Ok(())
}
