//! `workflows/assay.yml.tmpl`, the pipeline workflow consumers install (#60 ST-15, 02 §1, §5,
//! 03 §4a, 04 §1, 05 §4, 0006): its triggers, the guard before every secret, the run-attempt
//! guard, the expression and concurrency rules, least-privilege tokens, pinned and cache-free
//! actions, the bundle flow, and argv taken only from [`PIPELINE`].

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use assay_cli::OutputName;

use super::doctor_prerequisites::{Fake, written};
use super::pipeline_argv::{Invocation, PIPELINE};
use super::workflow::{
    job_body, job_entries, job_value, jobs, permissions, steps, template, top_level_block,
};
use super::workflow_template::{
    ASSAY_FETCH, ActionPins, Expected, Minted, action_pins, all_steps, app_tokens, assay_step,
    identities, ledger_token, needs, raw_run_lines, step_identities, terms, token_variable,
    verdict_token,
};

use super::workflow_template::Expected::{
    Assay, Checkout, Download, Fetch, Ledger, Node, Temper, Token, Upload,
};

fn pipeline() -> io::Result<String> {
    template("assay.yml.tmpl")
}

/// The jobs in file order (04 §1).
const JOBS: [&str; 10] = [
    "guard",
    "open",
    "plan",
    "gates-static",
    "gates-lint",
    "gates-test",
    "admit",
    "review",
    "release",
    "decide",
];

/// The jobs that hold a secret or an environment (05 §5).
const PRIVILEGED: [&str; 4] = ["open", "admit", "release", "decide"];

/// The jobs that run on attempt 1 only (02 §1, R3-05): those holding an App key, and review,
/// which takes the model secret with M2.
const FIRST_ATTEMPT_ONLY: [&str; 5] = ["open", "admit", "review", "release", "decide"];

/// The guard's gate, a top-level term of every privileged job's `if` (0006).
const GUARD_RAN: &str = "needs.guard.outputs.run == 'true'";

/// The run-attempt guard (02 §1, T22).
const FIRST_ATTEMPT: &str = "github.run_attempt == 1";

/// The jobs that may be cancelled by a newer push, and so carry a concurrency group (02 §5).
const GROUPED: [&str; 4] = ["gates-static", "gates-lint", "gates-test", "review"];

/// The only `cancel-in-progress` (02 §5).
const CANCEL: &str = "cancel-in-progress: ${{ github.event.action == 'synchronize' }}";

#[test]
fn triggers_are_the_pipeline_events_with_no_branch_filter_and_a_pr_only_dispatch() -> io::Result<()>
{
    let workflow = pipeline()?;

    let on = top_level_block(&workflow, "on");

    assert_eq!(
        on,
        [
            "on:",
            "  pull_request_target:",
            "    types: [opened, synchronize, reopened, ready_for_review, edited]",
            "  workflow_dispatch:",
            "    inputs:",
            "      pr:",
            "        description: The pull request to run the pipeline for, by number.",
            "        required: true",
            "        type: string",
        ]
    );
    Ok(())
}

#[test]
fn the_jobs_are_the_pipeline_s_in_order_with_guard_first() -> io::Result<()> {
    let workflow = pipeline()?;

    let names = jobs(&workflow)
        .iter()
        .map(|(job, _)| *job)
        .collect::<Vec<_>>();

    assert_eq!(names, JOBS);
    Ok(())
}

/// Each job's steps in file order, with [`Expected::Assay`] rows of [`PIPELINE`]; nothing else
/// may run in a job, so an added `run: curl … | sh` or download fails.
const STEPS: [(&str, &[Expected]); 10] = [
    ("guard", &[Checkout, Fetch, Assay(Some("guard"), 0)]),
    (
        "open",
        &[
            Checkout,
            Fetch,
            Ledger,
            Token("verdict-token"),
            Token("ledger-token"),
            Assay(None, 1),
        ],
    ),
    (
        "plan",
        &[Checkout, Fetch, Assay(Some("plan"), 2), Upload(None)],
    ),
    (
        "gates-static",
        &[
            Checkout,
            Checkout,
            Fetch,
            Temper,
            Node,
            Assay(Some("gates"), 3),
            Upload(None),
        ],
    ),
    (
        "gates-lint",
        &[
            Checkout,
            Checkout,
            Fetch,
            Temper,
            Node,
            Assay(Some("gates"), 4),
            Upload(None),
        ],
    ),
    (
        "gates-test",
        &[
            Checkout,
            Checkout,
            Fetch,
            Assay(Some("gates"), 5),
            Upload(None),
        ],
    ),
    (
        "admit",
        &[
            Checkout,
            Fetch,
            Ledger,
            Token("verdict-token"),
            Token("ledger-token"),
            Download,
            Assay(Some("admit"), 6),
            Upload(None),
        ],
    ),
    ("review", &[Checkout, Fetch, Assay(Some("review"), 7)]),
    (
        "release",
        &[
            Checkout,
            Fetch,
            Ledger,
            Token("ledger-token"),
            Assay(None, 8),
        ],
    ),
    (
        "decide",
        &[
            Checkout,
            Fetch,
            Ledger,
            Token("verdict-token"),
            Token("ledger-token"),
            Download,
            Assay(Some("decide"), 9),
            Upload(Some("persist")),
            Assay(None, 10),
        ],
    ),
];

#[test]
fn each_job_runs_exactly_its_steps_in_order() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let read = step_identities(&jobs);

    let expected = STEPS
        .iter()
        .map(|(job, steps)| (*job, identities(PIPELINE, steps)))
        .collect::<Vec<_>>();
    assert_eq!(read, expected);
    Ok(())
}

#[test]
fn guard_holds_no_secret_variable_environment_or_app_token() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);
    let guard = job_body(&jobs, "guard");

    let held = guard
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("secrets")
                || lower.contains("vars.")
                || line.trim_start().starts_with("environment:")
                || line.contains("create-github-app-token")
        })
        .collect::<Vec<_>>();

    assert!(!guard.is_empty(), "the template has no guard job");
    assert_eq!(held, Vec::<&&str>::new(), "guard holds these");
    Ok(())
}

#[test]
fn every_job_holding_a_secret_or_an_environment_needs_guard_and_is_gated_on_its_run()
-> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

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
    let ungated = privileged
        .iter()
        .filter(|job| {
            let body = job_body(&jobs, job);
            !needs(body).contains(&"guard")
                || !job_value(body, "if")
                    .is_some_and(|condition| terms(condition).contains(&GUARD_RAN))
        })
        .collect::<Vec<_>>();
    let environments = privileged
        .iter()
        .map(|job| job_value(job_body(&jobs, job), "environment"))
        .collect::<Vec<_>>();

    assert_eq!(privileged, PRIVILEGED);
    assert_eq!(
        ungated,
        Vec::<&&str>::new(),
        "privileged jobs not gated on guard"
    );
    assert_eq!(environments, [Some("assay-publish"); 4]);
    Ok(())
}

#[test]
fn each_job_runs_exactly_when_and_after_what_the_pipeline_needs() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let read = jobs
        .iter()
        .map(|(job, body)| (*job, (needs(body), job_value(body, "if"))))
        .collect::<BTreeMap<_, _>>();

    let first_run = "needs.guard.outputs.run == 'true' && github.run_attempt == 1";
    let after_plan = || (vec!["guard", "plan"], None);
    let expected = BTreeMap::from([
        ("guard", (vec![], None)),
        ("open", (vec!["guard"], Some(first_run))),
        ("plan", (vec!["guard"], Some(GUARD_RAN))),
        ("gates-static", after_plan()),
        ("gates-lint", after_plan()),
        ("gates-test", after_plan()),
        ("admit", (vec!["guard", "plan"], Some(first_run))),
        (
            "review",
            (
                vec!["guard", "admit"],
                Some(
                    "needs.guard.outputs.run == 'true' && needs.admit.outputs.review == 'true' \
                     && github.run_attempt == 1",
                ),
            ),
        ),
        (
            "release",
            (
                vec!["guard", "admit", "review"],
                Some(
                    "always() && needs.guard.outputs.run == 'true' \
                     && needs.admit.outputs.slot != '' && github.run_attempt == 1",
                ),
            ),
        ),
        (
            "decide",
            (
                JOBS[..9].to_vec(),
                Some("always() && needs.guard.outputs.run == 'true' && github.run_attempt == 1"),
            ),
        ),
    ]);
    assert_eq!(read, expected);
    Ok(())
}

#[test]
fn every_job_holding_a_token_that_writes_or_a_secret_runs_on_the_first_attempt_only()
-> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let guarded = jobs
        .iter()
        .filter(|(_, body)| {
            job_value(body, "if").is_some_and(|condition| terms(condition).contains(&FIRST_ATTEMPT))
        })
        .map(|(job, _)| *job)
        .collect::<Vec<_>>();
    let decide = job_value(job_body(&jobs, "decide"), "if");

    assert_eq!(guarded, FIRST_ATTEMPT_ONLY);
    assert!(
        decide.is_some_and(|condition| condition.starts_with("always() && ")),
        "decide: {decide:?}"
    );
    Ok(())
}

#[test]
fn no_run_holds_an_expression_on_any_line_comments_included() -> io::Result<()> {
    let workflow = pipeline()?;

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
fn a_run_block_s_comment_line_is_read_as_run_text() {
    let text = "jobs:\n  a:\n    steps:\n      - run: |\n          # ${{ x }}\n          echo\n        env:\n          A: ${{ y }}\n      - run: echo ${{ z }}\n      - run: |+\n          ${{ p }}\n      - run: >- # note\n          ${{ q }}\n";

    let runs = raw_run_lines(text);

    assert_eq!(
        runs,
        [
            (4, "      - run: |"),
            (5, "          # ${{ x }}"),
            (6, "          echo"),
            (9, "      - run: echo ${{ z }}"),
            (10, "      - run: |+"),
            (11, "          ${{ p }}"),
            (12, "      - run: >- # note"),
            (13, "          ${{ q }}"),
        ]
    );
}

#[test]
fn concurrency_expressions_appear_only_in_the_cancellable_jobs_concurrency() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let concurrency = jobs
        .iter()
        .map(|(job, body)| (*job, job_entries(body, "concurrency")))
        .filter(|(_, entries)| !entries.is_empty())
        .collect::<BTreeMap<_, _>>();
    let markers = [
        "github.event.action",
        "cancel-in-progress",
        "assay-noop",
        "group:",
    ];
    let elsewhere = workflow
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| markers.iter().any(|marker| line.contains(marker)))
        .filter(|line| {
            !concurrency
                .values()
                .any(|entries| entries.contains(&line.trim()))
        })
        .collect::<Vec<_>>();

    let expected = GROUPED
        .iter()
        .map(|job| {
            let group = format!(
                "group: ${{{{ needs.guard.outputs.run == 'true' && format('assay-{{0}}-{{1}}-{job}', \
                 github.repository_id, needs.guard.outputs.pr) || format('assay-noop-{{0}}-{job}', \
                 github.run_id) }}}}"
            );
            (*job, vec![group, CANCEL.to_owned()])
        })
        .collect::<BTreeMap<_, _>>();
    let read = concurrency
        .iter()
        .map(|(job, entries)| {
            (
                *job,
                entries.iter().map(|&entry| entry.to_owned()).collect(),
            )
        })
        .collect::<BTreeMap<_, Vec<String>>>();
    assert_eq!(read, expected);
    assert_eq!(
        top_level_block(&workflow, "concurrency"),
        Vec::<&str>::new()
    );
    assert_eq!(elsewhere, Vec::<&str>::new(), "outside `concurrency:`");
    Ok(())
}

#[test]
fn every_job_is_granted_exactly_its_permissions_and_the_workflow_none() -> io::Result<()> {
    let workflow = pipeline()?;

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
            grants(&["contents", "packages", "pull-requests"]),
        ),
        ("open".to_owned(), grants(&["contents", "packages"])),
        (
            "plan".to_owned(),
            grants(&["contents", "packages", "pull-requests"]),
        ),
        (
            "gates-static".to_owned(),
            grants(&["actions", "contents", "packages"]),
        ),
        ("gates-lint".to_owned(), grants(&["contents", "packages"])),
        ("gates-test".to_owned(), grants(&["contents", "packages"])),
        (
            "admit".to_owned(),
            grants(&["actions", "contents", "packages"]),
        ),
        (
            "review".to_owned(),
            grants(&["actions", "contents", "packages"]),
        ),
        ("release".to_owned(), grants(&["contents", "packages"])),
        (
            "decide".to_owned(),
            grants(&["actions", "contents", "packages", "pull-requests"]),
        ),
    ]);
    assert_eq!(granted, expected);
    assert_eq!(workflow_default, ["permissions: {}"]);
    Ok(())
}

/// The tokens 05 §4 narrows each job to (W2: `actions: read` for open's `created_at` read and
/// `contents: read` for its compare, 03 §4a), each a [`verdict_token`] or a [`ledger_token`].
fn expected_tokens() -> Minted<'static> {
    BTreeMap::from([
        (
            ("open", "verdict-token"),
            verdict_token(&[
                ("permission-actions", "read"),
                ("permission-checks", "write"),
                ("permission-contents", "read"),
            ]),
        ),
        (("open", "ledger-token"), ledger_token()),
        (
            ("admit", "verdict-token"),
            verdict_token(&[
                ("permission-checks", "read"),
                ("permission-pull-requests", "read"),
            ]),
        ),
        (("admit", "ledger-token"), ledger_token()),
        (("release", "ledger-token"), ledger_token()),
        (
            ("decide", "verdict-token"),
            verdict_token(&[
                ("permission-checks", "write"),
                ("permission-contents", "read"),
                ("permission-pull-requests", "write"),
            ]),
        ),
        (("decide", "ledger-token"), ledger_token()),
    ])
}

#[test]
fn each_app_token_is_narrowed_to_one_repository_and_its_job_s_scopes() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let minted = app_tokens(&jobs);

    assert_eq!(minted, expected_tokens());
    Ok(())
}

#[test]
fn every_checkout_drops_its_credentials_and_only_gates_jobs_check_out_the_head() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let checkouts = all_steps(&jobs)
        .into_iter()
        .filter(|(_, step)| {
            step.value("uses")
                .is_some_and(|action| action.starts_with("actions/checkout@"))
        })
        .map(|(job, step)| {
            (
                job,
                step.entry("with", "ref").unwrap_or_default(),
                step.entry("with", "path").unwrap_or_default(),
                step.entry("with", "persist-credentials")
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();

    let base = "${{ needs.guard.outputs.base_sha }}";
    let head = "${{ needs.guard.outputs.head_sha }}";
    let mut expected = vec![("guard", "${{ github.sha }}", "base", "false")];
    for job in &JOBS[1..] {
        expected.push((job, base, "base", "false"));
        if job.starts_with("gates-") {
            expected.push((job, head, "pr", "false"));
        }
    }
    assert_eq!(checkouts, expected);
    Ok(())
}

#[test]
fn every_action_is_pinned_allowlisted_and_shares_the_repository_s_pin() -> io::Result<()> {
    let workflow = pipeline()?;

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

/// The inputs of the step fetching the pinned temper copy (D8 A, W6), installed outside the
/// workspace a PR checkout shares.
const TEMPER_FETCH: [(&str, &str); 5] = [
    ("repository", "ghcr.io/wet-ink-corporation/temper"),
    ("config", "base/.assay/plugins.toml"),
    ("selector", "plugin.temper"),
    ("mode", "\"0644\""),
    ("out-dir", "${{ runner.temp }}/temper"),
];

#[test]
fn every_fetch_by_digest_reads_its_pin_from_base_config_with_fixed_inputs() -> io::Result<()> {
    let workflow = pipeline()?;
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

    let mut expected = Vec::new();
    for job in JOBS {
        expected.push((job, None, ASSAY_FETCH.to_vec()));
        if matches!(job, "gates-static" | "gates-lint") {
            expected.push((job, Some("temper"), TEMPER_FETCH.to_vec()));
        }
    }
    assert_eq!(fetches, expected);
    Ok(())
}

#[test]
fn review_is_bounded_to_forty_minutes() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let timeout = job_value(job_body(&jobs, "review"), "timeout-minutes")
        .and_then(|minutes| minutes.parse::<u32>().ok());

    assert!(timeout.is_some_and(|minutes| minutes <= 40), "{timeout:?}");
    Ok(())
}

#[test]
fn decide_uploads_its_bundle_between_decide_and_publish() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);
    let steps = steps(job_body(&jobs, "decide"));
    let [decide, publish] =
        [&PIPELINE[PIPELINE.len() - 2], &PIPELINE[PIPELINE.len() - 1]].map(Invocation::run_line);

    let at = |run: &str| {
        steps
            .iter()
            .position(|step| step.run().as_deref() == Some(run))
    };
    let d = at(&decide).expect("decide runs `assay decide`");
    let p = steps
        .iter()
        .position(|step| step.value("id") == Some("persist"))
        .expect("decide has a `persist` step");
    let u = at(&publish).expect("decide runs `assay publish`");

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
            ("name", "${{ needs.plan.outputs.artifact_decide }}"),
            ("path", "decide-out/run-inputs.json"),
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
fn admit_and_decide_read_artifacts_from_one_download_of_assay_artifacts() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let downloads = all_steps(&jobs)
        .into_iter()
        .filter(|(_, step)| {
            step.value("uses")
                .is_some_and(|action| action.starts_with("actions/download-artifact@"))
        })
        .map(|(job, step)| (job, step.entries("with")))
        .collect::<Vec<_>>();
    let before_assay = ["admit", "decide"].map(|job| {
        let steps = steps(job_body(&jobs, job));
        let download = steps.iter().position(|step| {
            step.value("uses")
                .is_some_and(|action| action.starts_with("actions/download-artifact@"))
        });
        let assay = steps
            .iter()
            .position(|step| step.run().is_some_and(|run| run.starts_with("bin/assay")));
        matches!((download, assay), (Some(d), Some(a)) if d < a)
    });

    let with = || vec![("pattern", "assay-*"), ("path", "artifacts")];
    assert_eq!(downloads, [("admit", with()), ("decide", with())]);
    assert_eq!(before_assay, [true, true]);
    Ok(())
}

#[test]
fn each_artifact_is_uploaded_under_the_name_plan_published_for_its_job() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let uploads = all_steps(&jobs)
        .into_iter()
        .filter(|(_, step)| {
            step.value("uses")
                .is_some_and(|action| action.starts_with("actions/upload-artifact@"))
        })
        .map(|(job, step)| {
            (
                job,
                step.entry("with", "name").unwrap_or_default(),
                step.entry("with", "path").unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();

    let gates = |job: &'static str, artifact: &'static str| (job, artifact, "gates-out/*.json");
    assert_eq!(
        uploads,
        [
            (
                "plan",
                "${{ steps.plan.outputs.artifact_plan }}",
                "plan-out/run.json"
            ),
            gates(
                "gates-static",
                "${{ needs.plan.outputs.artifact_gates_static }}"
            ),
            gates(
                "gates-lint",
                "${{ needs.plan.outputs.artifact_gates_lint }}"
            ),
            gates(
                "gates-test",
                "${{ needs.plan.outputs.artifact_gates_test }}"
            ),
            (
                "admit",
                "${{ needs.plan.outputs.artifact_admit }}",
                "admit-out/"
            ),
            (
                "decide",
                "${{ needs.plan.outputs.artifact_decide }}",
                "decide-out/run-inputs.json"
            ),
        ]
    );
    Ok(())
}

#[test]
fn every_assay_step_runs_an_argv_of_the_shared_table_with_exactly_its_variables() -> io::Result<()>
{
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let assay_lines = workflow
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && line.contains("bin/assay"))
        .map(str::trim)
        .collect::<Vec<_>>();
    let missing = PIPELINE
        .iter()
        .filter(|invocation| assay_step(&jobs, invocation).is_none())
        .map(Invocation::run_line)
        .collect::<Vec<_>>();
    let variables = PIPELINE
        .iter()
        .filter_map(|invocation| {
            let step = assay_step(&jobs, invocation)?;
            let set = step
                .entries("env")
                .into_iter()
                .map(|(name, _)| name)
                .collect::<BTreeSet<_>>();
            let expected = invocation.env.iter().copied().collect::<BTreeSet<_>>();
            (set != expected).then(|| (invocation.run_line(), set))
        })
        .collect::<Vec<_>>();

    let expected = PIPELINE
        .iter()
        .map(|invocation| format!("run: {}", invocation.run_line()))
        .collect::<Vec<_>>();
    assert_eq!(assay_lines, expected, "every bin/assay line, in file order");
    assert_eq!(missing, Vec::<String>::new(), "not run in their job");
    assert_eq!(
        variables,
        Vec::<(String, BTreeSet<&str>)>::new(),
        "env other than the table's"
    );
    Ok(())
}

/// What each assay step's variable must be, where it names a token or the needs.
fn expected_variable(job: &str, name: &str) -> Option<&'static str> {
    Some(match (job, name) {
        ("guard", "ASSAY_RUN_TOKEN") => {
            "${{ github.event_name == 'workflow_dispatch' && github.token || '' }}"
        }
        ("release", "ASSAY_NEEDS") => {
            "${{ format('{{\"admit\":{0},\"review\":{1}}}', toJSON(needs.admit), toJSON(needs.review)) }}"
        }
        (_, "ASSAY_NEEDS") => "${{ toJSON(needs) }}",
        (_, "ASSAY_BASE_SHA") => "${{ needs.guard.outputs.base_sha }}",
        (_, "ASSAY_HEAD_SHA") => "${{ needs.guard.outputs.head_sha }}",
        (_, "ASSAY_MERGE_BASE") => "${{ needs.plan.outputs.merge_base }}",
        (_, "ASSAY_RUN_KEY") => "${{ needs.plan.outputs.run_key }}",
        (_, "ASSAY_NODE") => "${{ steps.node.outputs.path }}",
        (_, "ASSAY_TEMPER") => "${{ steps.temper.outputs.dir }}",
        (_, name) => return token_variable(name),
    })
}

#[test]
fn every_assay_variable_comes_from_its_one_source() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let wrong = PIPELINE
        .iter()
        .filter_map(|invocation| Some((invocation, assay_step(&jobs, invocation)?)))
        .flat_map(|(invocation, step)| {
            step.entries("env")
                .into_iter()
                .filter(|(name, value)| expected_variable(invocation.job, name) != Some(*value))
                .map(|(name, value)| format!("{}: {name}: {value}", invocation.job))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}

#[test]
fn each_job_publishes_exactly_the_outputs_its_command_writes() -> io::Result<()> {
    let workflow = pipeline()?;
    let jobs = jobs(&workflow);

    let published = jobs
        .iter()
        .map(|(job, body)| (*job, job_entries(body, "outputs")))
        .filter(|(_, outputs)| !outputs.is_empty())
        .collect::<BTreeMap<_, _>>();

    // The binary's own closed sets, so an output it gains or renames fails here.
    let from = |step: &str, names: &[OutputName]| {
        names
            .iter()
            .map(|name| {
                let name = name.as_str();
                format!("{name}: ${{{{ steps.{step}.outputs.{name} }}}}")
            })
            .collect::<Vec<_>>()
    };
    let expected = BTreeMap::from([
        ("guard", from("guard", &OutputName::GUARD)),
        ("plan", from("plan", &OutputName::PLAN)),
        ("gates-static", from("gates", &OutputName::GATES)),
        ("gates-lint", from("gates", &OutputName::GATES)),
        ("gates-test", from("gates", &OutputName::GATES)),
        ("admit", from("admit", &OutputName::ADMIT)),
        ("review", from("review", &OutputName::REVIEW)),
    ]);
    let read = published
        .iter()
        .map(|(job, outputs)| (*job, outputs.iter().map(|&o| o.to_owned()).collect()))
        .collect::<BTreeMap<_, Vec<String>>>();
    assert_eq!(read, expected);
    Ok(())
}

#[test]
fn doctor_s_prerequisite_checks_pass_with_the_template_installed() -> io::Result<()> {
    let mut repository = Fake::repository();
    repository
        .files
        .insert(".github/workflows/assay.yml", pipeline()?);

    let (passed, out) = written(&repository);

    assert!(passed, "{out}");
    Ok(())
}
