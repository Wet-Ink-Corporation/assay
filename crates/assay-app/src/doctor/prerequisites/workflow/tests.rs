use std::fs;
use std::path::Path;

use super::*;

const SHA: &str = "3d3c42e5aac5ba805825da76410c181273ba90b1";

#[test]
fn the_repositorys_own_workflows_are_classified_and_pinned() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows");
    let mut read = 0_usize;
    for entry in fs::read_dir(&dir).expect("the workflows directory is readable") {
        let path = entry.expect("a directory entry").path();
        let text = fs::read_to_string(&path).expect("a workflow is UTF-8");

        let workflow =
            Workflow::read(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));

        assert!(!workflow.triggers.is_empty(), "{}", path.display());
        assert!(!workflow.uses.is_empty(), "{}", path.display());
        let unpinned: Vec<_> = workflow
            .uses
            .iter()
            .filter(|uses| !is_pinned(uses))
            .collect();
        assert!(unpinned.is_empty(), "{}: {unpinned:?}", path.display());
        read = read.saturating_add(1);
    }
    assert!(
        read >= 3,
        "release.yml, rust.yml and warm-target.yml are read"
    );
}

fn workflow(text: &str) -> Workflow {
    Workflow::read(text).expect("the workflow is read")
}

/// The one job of `jobs` text under `permissions` (workflow level, or none).
fn job(permissions: &str, job: &str) -> Job {
    let text = format!("on: push\n{permissions}jobs:\n  only:\n{job}");
    let mut jobs = workflow(&text).jobs;
    assert_eq!(jobs.len(), 1, "{text}");
    jobs.remove(0)
}

#[test]
fn triggers_are_read_in_each_form() {
    let cases = [
        (
            "on: pull_request_target\njobs: {}\n",
            vec!["pull_request_target"],
        ),
        (
            "on: [push, 'issue_comment']\njobs: {}\n",
            vec!["push", "issue_comment"],
        ),
        (
            "\"on\":\n  pull_request:\n    types: [opened]\n  schedule:\n    - cron: '1 * * * *'\njobs: {}\n",
            vec!["pull_request", "schedule"],
        ),
        (
            "on:\n- push\n- pull_request_review\njobs: {}\n",
            vec!["push", "pull_request_review"],
        ),
    ];

    for (text, expected) in cases {
        assert_eq!(workflow(text).triggers, expected, "{text}");
    }
}

#[test]
fn a_workflow_without_triggers_or_jobs_is_refused() {
    assert_eq!(
        Workflow::read("jobs: {}\n"),
        Err(WorkflowError::Shape("no `on` triggers"))
    );
    assert_eq!(
        Workflow::read("on: push\n"),
        Err(WorkflowError::Shape("no `jobs` mapping"))
    );
    assert_eq!(
        Workflow::read("on: {}\njobs: {}\n"),
        Err(WorkflowError::Shape("no `on` triggers"))
    );
}

#[test]
fn a_refusal_of_the_reader_is_kept() {
    let refused = Workflow::read("on: &a push\njobs: {}\n");

    assert!(
        matches!(&refused, Err(WorkflowError::Unreadable(refusal)) if refusal.reason() == YamlRefusal::Indicator),
        "{refused:?}"
    );
}

#[test]
fn every_uses_of_steps_and_jobs_is_collected() {
    let text = format!(
        "on: push\njobs:\n  a:\n    steps:\n      - uses: actions/checkout@{SHA}\n      - run: echo\n  b:\n    uses: ./.github/workflows/x.yml\n"
    );

    assert_eq!(
        workflow(&text).uses,
        vec![
            format!("actions/checkout@{SHA}"),
            "./.github/workflows/x.yml".to_owned()
        ]
    );
}

#[test]
fn only_full_commit_shas_local_paths_and_image_digests_are_pinned() {
    let digest = "a".repeat(64);
    let pinned_cases = [
        format!("actions/checkout@{SHA}"),
        format!("github/codeql-action/init@{SHA}"),
        "./.github/actions/warm-target".to_owned(),
        format!("docker://alpine@sha256:{digest}"),
    ];
    let unpinned_cases = [
        "actions/checkout@v4".to_owned(),
        "actions/checkout@3d3c42e".to_owned(),
        format!("actions/checkout@{}", SHA.to_uppercase()),
        "actions/checkout".to_owned(),
        format!("checkout@{SHA}"),
        format!("actions//checkout@{SHA}"),
        "docker://alpine:3".to_owned(),
        format!("docker://@sha256:{digest}"),
    ];

    for uses in &pinned_cases {
        assert!(is_pinned(uses), "{uses}");
    }
    for uses in &unpinned_cases {
        assert!(!is_pinned(uses), "{uses}");
    }
}

#[test]
fn the_guard_counts_only_as_a_top_level_conjunct() {
    let guarded = [
        "github.run_attempt == 1",
        "${{ github.run_attempt == 1 }}",
        "always() && github.run_attempt == 1",
        "contains(github.event.action, 'x || y') && github.run_attempt == 1",
    ];
    let unguarded = [
        "github.run_attempt == 1 || true",
        "always() || github.run_attempt == 1 && true",
        "(github.run_attempt == 1)",
        "'github.run_attempt == 1'",
        "github.run_attempt == 2",
        "${{ github.run_attempt == 1 }} && ${{ true }}",
        "x == 'a && github.run_attempt == 1'",
        "github.run_attempt == 1 }}",
    ];

    for condition in guarded {
        assert!(is_guarded(condition), "{condition}");
    }
    for condition in unguarded {
        assert!(!is_guarded(condition), "{condition}");
    }
}

#[test]
fn a_jobs_guard_is_its_if() {
    assert!(job("", "    if: github.run_attempt == 1\n    runs-on: x\n").guarded);
    assert!(!job("", "    runs-on: x\n").guarded);
    assert!(!job("", "    if: |\n      github.run_attempt == 1\n").guarded);
}

#[test]
fn what_a_job_holds_is_read_from_permissions_environment_and_secrets() {
    let read_only = "permissions:\n  contents: read\n";
    let cases = [
        (read_only, "    runs-on: x\n", None),
        ("", "    runs-on: x\n", Some(Holding::WriteToken)),
        (
            "permissions: write-all\n",
            "    runs-on: x\n",
            Some(Holding::WriteToken),
        ),
        ("permissions: read-all\n", "    runs-on: x\n", None),
        ("permissions: {}\n", "    runs-on: x\n", None),
        (
            read_only,
            "    permissions:\n      checks: write\n",
            Some(Holding::WriteToken),
        ),
        ("permissions: write-all\n", "    permissions: {}\n", None),
        (
            read_only,
            "    environment: assay-publish\n",
            Some(Holding::Environment),
        ),
        (
            read_only,
            "    steps:\n      - run: |\n          echo ${{ secrets.KEY }}\n",
            Some(Holding::Secret),
        ),
        (
            read_only,
            "    uses: ./x.yml\n    secrets: inherit\n",
            Some(Holding::Secret),
        ),
    ];

    for (permissions, text, expected) in cases {
        assert_eq!(
            job(permissions, text).holds,
            expected,
            "{permissions}{text}"
        );
    }
}

#[test]
fn a_secret_in_the_workflows_env_is_held_by_every_job() {
    let text =
        "on: push\npermissions: {}\nenv:\n  KEY: ${{ secrets.KEY }}\njobs:\n  a:\n    runs-on: x\n";

    assert_eq!(workflow(text).jobs[0].holds, Some(Holding::Secret));
}

/// The reader's refusal of `text`, which must be one.
fn refusal(text: &str) -> YamlRefusal {
    match Workflow::read(text) {
        Err(WorkflowError::Unreadable(refusal)) => refusal.reason(),
        other => panic!("{text:?} is not refused by the reader: {other:?}"),
    }
}

#[test]
fn a_trigger_or_uses_hidden_behind_a_break_yaml_reads_is_refused() {
    let trigger = "on:\n  push:\n    branches: x\r  pull_request:\n    types: [opened]\njobs: {}\n";
    let uses = "on: push\njobs:\n  a:\n    steps:\n      - name: x\r        uses: evil/act@main\n";

    assert_eq!(refusal(trigger), YamlRefusal::Character);
    assert_eq!(refusal(uses), YamlRefusal::Character);
    for separator in ['\u{85}', '\u{2028}', '\u{2029}'] {
        let text = format!(
            "on: push\njobs:\n  a:\n    steps:\n      - name: x{separator}        uses: evil/act@main\n"
        );
        assert_eq!(refusal(&text), YamlRefusal::Character, "{separator:?}");
    }
}

#[test]
fn a_uses_or_trigger_after_a_tab_is_refused() {
    let uses = "on: push\njobs:\n  a:\n    steps:\n      - uses:\tevil/act@main\n";
    let trigger = "on: pull_request\t# ci\njobs: {}\n";

    assert_eq!(refusal(uses), YamlRefusal::Tab);
    assert_eq!(refusal(trigger), YamlRefusal::Tab);
}

#[test]
fn a_step_that_is_not_a_mapping_is_refused() {
    for step in ["- uses evil/act@main", "-", "- [a]"] {
        let text = format!("on: push\njobs:\n  a:\n    steps:\n      {step}\n");

        assert_eq!(
            Workflow::read(&text),
            Err(WorkflowError::Shape("a step is not a mapping")),
            "{step}"
        );
    }
}

#[test]
fn a_secret_is_seen_in_any_ascii_case() {
    let in_job = "on: push\npermissions: {}\njobs:\n  a:\n    steps:\n      - run: echo ${{ SECRETS.KEY }}\n";
    let in_env =
        "on: push\npermissions: {}\nenv:\n  K: ${{ Secrets.KEY }}\njobs:\n  a:\n    runs-on: x\n";

    assert_eq!(workflow(in_job).jobs[0].holds, Some(Holding::Secret));
    assert_eq!(workflow(in_env).jobs[0].holds, Some(Holding::Secret));
}

#[test]
fn permissions_that_cannot_be_classified_are_refused() {
    for permissions in ["permissions: admin\n", "permissions:\n  contents: maybe\n"] {
        let text = format!("on: push\n{permissions}jobs: {{}}\n");

        assert_eq!(
            Workflow::read(&text),
            Err(WorkflowError::Shape(
                "`permissions` is not read, write or none"
            )),
            "{permissions}"
        );
    }
}
