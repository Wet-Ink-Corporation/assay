//! `workflows/assay-decide.yml.tmpl`, the decide-only workflow consumers install (#61 DO-13,
//! 02 §1, §5, 03 §4a, 05 §4, 0006): its triggers, the secretless guard and its event filter, the
//! first-attempt matrix job in a non-cancelling per-PR queue, the expression rules,
//! least-privilege tokens, pinned and cache-free actions, the decision bundle's upload between
//! decide and publish, and argv taken only from [`DECIDE_ONLY`].

use std::collections::{BTreeMap, BTreeSet};
use std::{fs, io};

use assay_cli::OutputName;

use super::doctor_prerequisites::{Fake, written};
use super::pipeline_argv::{DECIDE_ONLY, Invocation};
use super::workflow::{
    job_body, job_entries, job_value, jobs, permissions, repository_root, steps, template,
    top_level_block,
};
use super::workflow_template::{
    ASSAY_FETCH, ActionPins, action_pins, all_steps, app_tokens, assay_step, ledger_token, needs,
    raw_run_lines, token_variable, verdict_token,
};

fn decide_only() -> io::Result<String> {
    template("assay-decide.yml.tmpl")
}

/// The jobs in file order.
const JOBS: [&str; 2] = ["guard", "redecide"];

/// The redecide job's `if`: the first attempt only (02 §1, T22), and only when guard selected
/// a PR.
const REDECIDE_IF: &str = "github.run_attempt == 1 && needs.guard.outputs.prs != '[]'";

/// 02 §5's filter for a comment that joins the real decide-only group: the condition of its
/// group expression, read from the spec so the template cannot drift from it.
fn comment_filter() -> io::Result<String> {
    let spec = fs::read_to_string(repository_root().join("docs/architecture/02-architecture.md"))?;
    spec.lines()
        .find_map(|line| {
            let (_, rest) = line.split_once("group: ${{ (")?;
            let (filter, _) = rest.split_once(") && format('assay-recheck-")?;
            Some(filter.to_owned())
        })
        .ok_or_else(|| io::Error::other("02 §5 has no decide-only group expression"))
}

#[test]
fn triggers_are_command_comments_label_changes_and_an_hourly_poll_off_the_hour() -> io::Result<()> {
    let workflow = decide_only()?;

    let on = top_level_block(&workflow, "on");
    let minute = on
        .iter()
        .find_map(|line| line.trim().strip_prefix("- cron: \""))
        .and_then(|cron| cron.split_once(' '))
        .and_then(|(minute, _)| minute.parse::<u8>().ok());
    let forbidden = workflow
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| {
            [
                "pull_request_review",
                "pull_request:",
                "workflow_dispatch",
                "branches",
            ]
            .iter()
            .any(|word| line.contains(word))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        on,
        [
            "on:",
            "  issue_comment:",
            "    types: [created]",
            "  pull_request_target:",
            "    types: [labeled, unlabeled]",
            "  schedule:",
            "    - cron: \"17 * * * *\"",
        ]
    );
    assert!(
        minute.is_some_and(|minute| (1..60).contains(&minute)),
        "{minute:?}"
    );
    assert_eq!(forbidden, Vec::<&str>::new());
    Ok(())
}

#[test]
fn the_jobs_are_guard_then_redecide() -> io::Result<()> {
    let workflow = decide_only()?;

    let names = jobs(&workflow)
        .iter()
        .map(|(job, _)| *job)
        .collect::<Vec<_>>();

    assert_eq!(names, JOBS);
    Ok(())
}

#[test]
fn guard_holds_no_secret_environment_or_app_token() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);
    let guard = job_body(&jobs, "guard");

    let held = guard
        .iter()
        .filter(|line| {
            line.to_ascii_lowercase().contains("secrets")
                || line.trim_start().starts_with("environment:")
                || line.contains("create-github-app-token")
        })
        .collect::<Vec<_>>();

    assert!(!guard.is_empty(), "the template has no guard job");
    assert_eq!(held, Vec::<&&str>::new(), "guard holds these");
    Ok(())
}

#[test]
fn redecide_needs_guard_and_decides_each_of_its_prs_on_the_first_attempt_in_assay_publish()
-> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);
    let redecide = job_body(&jobs, "redecide");

    let privileged = jobs
        .iter()
        .filter(|(_, body)| {
            body.iter().any(|line| {
                line.to_ascii_lowercase().contains("secrets")
                    || line.starts_with("    environment:")
            })
        })
        .map(|(job, _)| *job)
        .collect::<Vec<_>>();

    assert_eq!(privileged, ["redecide"]);
    assert_eq!(needs(redecide), ["guard"]);
    assert_eq!(job_value(redecide, "if"), Some(REDECIDE_IF));
    assert_eq!(job_value(redecide, "environment"), Some("assay-publish"));
    assert_eq!(
        job_entries(redecide, "strategy"),
        [
            "fail-fast: false",
            "matrix:",
            "pr: ${{ fromJSON(needs.guard.outputs.prs) }}",
        ]
    );
    Ok(())
}

#[test]
fn guard_runs_only_for_the_poll_an_assay_command_comment_or_an_assay_label() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);
    let comment = comment_filter()?;

    let condition = job_value(job_body(&jobs, "guard"), "if");

    let label = "github.event_name == 'pull_request_target' \
                 && startsWith(github.event.label.name, 'assay:') \
                 && github.event.sender.type != 'Bot'";
    let expected = format!("github.event_name == 'schedule' || ({comment}) || ({label})");
    assert!(
        comment.contains("startsWith(github.event.comment.body, '/assay ')"),
        "02 §5's filter: {comment}"
    );
    assert_eq!(condition, Some(expected.as_str()));
    Ok(())
}

#[test]
fn redecide_queues_per_pr_without_cancelling_and_the_filters_stay_in_concurrency_and_if()
-> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let concurrency = jobs
        .iter()
        .map(|(job, body)| (*job, job_entries(body, "concurrency")))
        .filter(|(_, entries)| !entries.is_empty())
        .collect::<BTreeMap<_, _>>();
    let markers = [
        "assay-recheck",
        "cancel-in-progress",
        "queue:",
        "group:",
        "startsWith(",
        "github.event.comment.body",
        "github.event.label.name",
        "sender.type",
    ];
    let elsewhere = workflow
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| markers.iter().any(|marker| line.contains(marker)))
        .filter(|line| {
            !line.starts_with("if: ") && !concurrency.values().any(|entries| entries.contains(line))
        })
        .collect::<Vec<_>>();

    let group = "group: ${{ format('assay-recheck-{0}-{1}', github.repository_id, matrix.pr) }}";
    assert_eq!(
        concurrency,
        BTreeMap::from([(
            "redecide",
            vec![group, "cancel-in-progress: false", "queue: max"]
        )])
    );
    assert_eq!(
        top_level_block(&workflow, "concurrency"),
        Vec::<&str>::new()
    );
    assert_eq!(
        elsewhere,
        Vec::<&str>::new(),
        "outside `concurrency:` and `if:`"
    );
    Ok(())
}

#[test]
fn no_run_holds_an_expression_on_any_line_comments_included() -> io::Result<()> {
    let workflow = decide_only()?;

    let runs = raw_run_lines(&workflow);
    let expressions = runs
        .iter()
        .filter(|(_, line)| line.contains("${{"))
        .collect::<Vec<_>>();

    assert!(
        runs.iter().any(|(_, line)| line.contains("tomllib")),
        "the `run: |` blocks were not read"
    );
    assert_eq!(expressions, Vec::<&(usize, &str)>::new());
    Ok(())
}

#[test]
fn every_job_is_granted_exactly_its_permissions_and_the_workflow_none() -> io::Result<()> {
    let workflow = decide_only()?;

    let granted = jobs(&workflow)
        .iter()
        .map(|(job, body)| ((*job).to_owned(), permissions(body)))
        .collect::<BTreeMap<_, _>>();
    let workflow_default = workflow
        .lines()
        .filter(|line| line.starts_with("permissions:"))
        .collect::<Vec<_>>();

    let grants = |scopes: &[&str]| Some(scopes.iter().map(|&s| format!("{s}: read")).collect());
    let expected = BTreeMap::from([
        (
            "guard".to_owned(),
            grants(&["checks", "contents", "packages", "pull-requests"]),
        ),
        (
            "redecide".to_owned(),
            grants(&["actions", "contents", "packages"]),
        ),
    ]);
    assert_eq!(granted, expected);
    assert_eq!(workflow_default, ["permissions: {}"]);
    Ok(())
}

#[test]
fn redecide_s_app_tokens_are_decide_s_narrowed_to_one_repository_each() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let minted = app_tokens(&jobs);

    let verdict = verdict_token(&[
        ("permission-checks", "write"),
        ("permission-contents", "read"),
        ("permission-pull-requests", "write"),
    ]);
    assert_eq!(
        minted,
        BTreeMap::from([
            (("redecide", "verdict-token"), verdict),
            (("redecide", "ledger-token"), ledger_token()),
        ])
    );
    Ok(())
}

#[test]
fn each_job_checks_out_only_base_config_at_the_event_s_commit_without_credentials() -> io::Result<()>
{
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let checkouts = all_steps(&jobs)
        .into_iter()
        .filter(|(_, step)| {
            step.value("uses")
                .is_some_and(|action| action.starts_with("actions/checkout@"))
        })
        .map(|(job, step)| (job, step.entries("with")))
        .collect::<Vec<_>>();

    let with = [
        ("ref", "${{ github.sha }}"),
        ("path", "base"),
        ("sparse-checkout", ".assay"),
        ("persist-credentials", "false"),
    ];
    assert_eq!(checkouts, JOBS.map(|job| (job, with.to_vec())));
    Ok(())
}

#[test]
fn every_action_is_pinned_allowlisted_and_shares_the_repository_s_pin() -> io::Result<()> {
    let workflow = decide_only()?;

    let ActionPins {
        raw,
        unpinned,
        unlisted,
        other_pins,
    } = action_pins(&workflow)?;

    assert!(
        raw.len() > JOBS.len(),
        "the template's actions were not read: {raw:?}"
    );
    assert_eq!(unpinned, Vec::<&str>::new(), "not pinned by SHA with a tag");
    assert_eq!(
        unlisted,
        Vec::<&str>::new(),
        "outside the cache-free allowlist"
    );
    assert_eq!(
        other_pins,
        Vec::<&str>::new(),
        "a pin other than .github/'s"
    );
    Ok(())
}

#[test]
fn each_job_fetches_assay_by_its_pin_in_base_config() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let fetches = all_steps(&jobs)
        .into_iter()
        .filter(|(_, step)| {
            step.value("uses").is_some_and(|action| {
                action.starts_with("Wet-Ink-Corporation/assay/.github/actions/fetch-by-digest@")
            })
        })
        .map(|(job, step)| (job, step.value("id"), step.entries("with")))
        .collect::<Vec<_>>();

    assert_eq!(fetches, JOBS.map(|job| (job, None, ASSAY_FETCH.to_vec())));
    Ok(())
}

#[test]
fn the_ledger_repository_is_read_from_base_config_as_the_pipeline_reads_it() -> io::Result<()> {
    let workflow = decide_only()?;
    let pipeline = template("assay.yml.tmpl")?;
    let ledger_steps = |text: &str| {
        all_steps(&jobs(text))
            .into_iter()
            .filter(|(_, step)| step.value("id") == Some("ledger"))
            .map(|(job, step)| (job.to_owned(), step.run()))
            .collect::<Vec<_>>()
    };

    let read = ledger_steps(&workflow);
    let decide = ledger_steps(&pipeline)
        .into_iter()
        .find_map(|(job, run)| if job == "decide" { run } else { None });

    assert!(decide.is_some(), "the pipeline's decide reads no ledger");
    assert_eq!(read, [("redecide".to_owned(), decide)]);
    Ok(())
}

#[test]
fn redecide_uploads_the_decision_bundle_between_decide_and_publish() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);
    let steps = steps(job_body(&jobs, "redecide"));
    let [_, decide, publish] =
        [&DECIDE_ONLY[0], &DECIDE_ONLY[1], &DECIDE_ONLY[2]].map(Invocation::run_line);

    let at = |run: &str| {
        steps
            .iter()
            .position(|step| step.run().as_deref() == Some(run))
    };
    let d = at(&decide).expect("redecide runs `assay decide-only decide`");
    let p = steps
        .iter()
        .position(|step| step.value("id") == Some("persist"))
        .expect("redecide has a `persist` step");
    let u = at(&publish).expect("redecide runs `assay decide-only publish`");

    assert!(d < p && p < u, "decide, upload, publish at {d}, {p}, {u}");
    assert_eq!(steps[d].value("id"), Some("decide"));
    assert!(
        steps[p]
            .value("uses")
            .is_some_and(|action| action.starts_with("actions/upload-artifact@")),
        "{:?}",
        steps[p]
    );
    assert_eq!(
        steps[p].value("if"),
        Some("steps.decide.outputs.bundle == 'true'")
    );
    assert_eq!(
        steps[p].entries("with"),
        [
            ("name", "${{ steps.decide.outputs.artifact }}"),
            ("path", "decide-out/run-decision.json"),
            ("retention-days", "90"),
            ("if-no-files-found", "error"),
        ]
    );
    assert_eq!(
        steps[u].entry("env", "ASSAY_PERSIST"),
        Some("${{ steps.persist.outcome }}")
    );
    assert_eq!(
        steps[u].value("if"),
        Some("${{ !cancelled() && steps.decide.outcome == 'success' }}")
    );
    Ok(())
}

#[test]
fn every_assay_step_runs_an_argv_of_the_shared_table_with_exactly_its_variables() -> io::Result<()>
{
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let assay_lines = workflow
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && line.contains("bin/assay"))
        .map(str::trim)
        .collect::<Vec<_>>();
    let variables = DECIDE_ONLY
        .iter()
        .map(|invocation| {
            let set = assay_step(&jobs, invocation).map(|step| {
                step.entries("env")
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect::<BTreeSet<_>>()
            });
            (invocation.run_line(), set)
        })
        .collect::<Vec<_>>();

    let expected_lines = DECIDE_ONLY
        .iter()
        .map(|invocation| format!("run: {}", invocation.run_line()))
        .collect::<Vec<_>>();
    let expected_variables = DECIDE_ONLY
        .iter()
        .map(|invocation| {
            let set = invocation.env.iter().copied().collect::<BTreeSet<_>>();
            (invocation.run_line(), Some(set))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        assay_lines, expected_lines,
        "every bin/assay line, in file order"
    );
    assert_eq!(
        variables, expected_variables,
        "run in its job with the table's env"
    );
    Ok(())
}

/// What each assay step's variable must be.
fn expected_variable(name: &str) -> Option<&'static str> {
    match name {
        "ASSAY_NEEDS" => Some("${{ toJSON(needs) }}"),
        "ASSAY_PR" => Some("${{ matrix.pr }}"),
        _ => token_variable(name),
    }
}

#[test]
fn every_assay_variable_comes_from_its_one_source() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let read = DECIDE_ONLY
        .iter()
        .filter_map(|invocation| Some((invocation, assay_step(&jobs, invocation)?)))
        .flat_map(|(invocation, step)| {
            step.entries("env")
                .into_iter()
                .map(|(name, value)| format!("{}: {name}: {value}", invocation.job))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let expected = DECIDE_ONLY
        .iter()
        .flat_map(|invocation| {
            invocation.env.iter().map(|name| {
                let value = expected_variable(name).unwrap_or("<no source>");
                format!("{}: {name}: {value}", invocation.job)
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(read, expected);
    Ok(())
}

#[test]
fn guard_publishes_exactly_its_outputs_and_redecide_none() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let published = jobs
        .iter()
        .map(|(job, body)| (*job, job_entries(body, "outputs")))
        .filter(|(_, outputs)| !outputs.is_empty())
        .collect::<BTreeMap<_, _>>();

    // The binary's own closed set, so an output it gains or renames fails here.
    let guard = OutputName::DECIDE_ONLY_GUARD
        .iter()
        .map(|name| {
            let name = name.as_str();
            format!("{name}: ${{{{ steps.guard.outputs.{name} }}}}")
        })
        .collect::<Vec<_>>();
    let read = published
        .iter()
        .map(|(job, outputs)| (*job, outputs.iter().map(|&o| o.to_owned()).collect()))
        .collect::<BTreeMap<_, Vec<String>>>();
    assert_eq!(read, BTreeMap::from([("guard", guard)]));
    Ok(())
}

#[test]
fn each_job_is_bounded_in_time() -> io::Result<()> {
    let workflow = decide_only()?;
    let jobs = jobs(&workflow);

    let timeouts = JOBS.map(|job| {
        job_value(job_body(&jobs, job), "timeout-minutes")
            .and_then(|minutes| minutes.parse::<u32>().ok())
    });

    assert_eq!(timeouts, [Some(10), Some(15)]);
    Ok(())
}

#[test]
fn doctor_s_prerequisite_checks_pass_with_both_templates_installed() -> io::Result<()> {
    let mut repository = Fake::repository();
    repository
        .files
        .insert(".github/workflows/assay.yml", template("assay.yml.tmpl")?);
    repository
        .files
        .insert(".github/workflows/assay-decide.yml", decide_only()?);

    let (passed, out) = written(&repository);

    assert!(passed, "{out}");
    Ok(())
}
