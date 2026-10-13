//! `assay status` refuses rather than models (wi-b5affa): every read that fails, every shape it
//! does not write or GitHub does not return, every bundle that does not authenticate, and every
//! count past its cap (H-13), each at the cap and one past it.

use std::assert_matches;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    BundleProblem, CheckStatus, CheckView, Detail, MAX_CHECK_RUNS, MAX_FINDINGS,
    MAX_PRIOR_COMMENTS, MAX_REASONS, ObservedConclusion, PullSelector, ReviewCommentView,
    StatusError, StatusReport, StatusStep, StatusUnreadable,
};
use assay_domain::{GateConclusion, GateFinding, GateOutcome, GateReport, Rubric, Severity};
use serde_json::Value;

use crate::plan_fakes::FakeError;
use crate::publish_support::{Run, stamped};
use crate::status::{
    Ask, COMMENT, document, failure_world, forged_verdict, fp, report, report_with, success_world,
};
use crate::status_fakes::{FakeGitHub, Stored, app, other_app, pull, run_url};

type Outcome = Result<StatusReport, StatusError<FakeError, FakeError>>;

fn refused(github: &FakeGitHub) -> Result<Outcome, Box<dyn Error>> {
    report(github, &Ask::new())
}

fn unreadable(outcome: &Outcome) -> Option<StatusUnreadable> {
    match outcome {
        Err(StatusError::Unreadable(what)) => Some(*what),
        _ => None,
    }
}

fn bundle_problem(outcome: Outcome) -> Option<String> {
    match outcome {
        Err(StatusError::Bundle(problem)) => Some(format!("{problem:?}")),
        _ => None,
    }
}

// --- choosing the PR ---

#[test]
fn a_pr_number_that_does_not_exist_is_refused() -> Result<(), Box<dyn Error>> {
    let ask = Ask {
        pull: PullSelector::Number(NonZeroU64::new(7).ok_or("not zero")?),
        ..Ask::new()
    };

    let outcome = report(&FakeGitHub::new(), &ask)?;

    assert_matches!(outcome, Err(StatusError::NoSuchPull { number }) if number.get() == 7);
    Ok(())
}

#[test]
fn a_branch_without_an_open_pr_is_refused() -> Result<(), Box<dyn Error>> {
    let ask = Ask {
        pull: PullSelector::Branch("elsewhere"),
        ..Ask::new()
    };

    assert_matches!(
        report(&FakeGitHub::new(), &ask)?,
        Err(StatusError::NoOpenPull)
    );
    Ok(())
}

#[test]
fn a_branch_with_two_open_prs_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = FakeGitHub::new();
    github.pulls.push(pull(43, crate::status_fakes::BRANCH));
    let ask = Ask {
        pull: PullSelector::Branch(crate::status_fakes::BRANCH),
        ..Ask::new()
    };

    assert_matches!(
        report(&github, &ask)?,
        Err(StatusError::AmbiguousBranch { count: 2 })
    );
    Ok(())
}

#[test]
fn every_failed_read_is_reported_with_its_step() -> Result<(), Box<dyn Error>> {
    let steps = [
        StatusStep::Pull,
        StatusStep::CheckRuns,
        StatusStep::ReviewComments,
        StatusStep::Viewer,
    ];
    for step in steps {
        let mut github = failure_world()?;
        github.failing = Some(step);

        let outcome = refused(&github)?;

        assert_matches!(outcome, Err(StatusError::Forge { step: failed, .. }) if failed == step);
    }
    let mut github = FakeGitHub::new();
    github.failing = Some(StatusStep::OpenPulls);
    let ask = Ask {
        pull: PullSelector::Branch(crate::status_fakes::BRANCH),
        ..Ask::new()
    };
    assert_matches!(
        report(&github, &ask)?,
        Err(StatusError::Forge {
            step: StatusStep::OpenPulls,
            ..
        })
    );
    Ok(())
}

#[test]
fn a_head_branch_that_does_not_read_is_refused_and_one_of_the_longest_length_is_kept()
-> Result<(), Box<dyn Error>> {
    let cases = [
        (String::new(), false),
        ("a\u{202e}b".to_owned(), false),
        ("a\nb".to_owned(), false),
        ("b".repeat(256), false),
        ("b".repeat(255), true),
    ];
    for (branch, kept) in cases {
        let mut github = FakeGitHub::new();
        github.pulls = vec![pull(42, &branch)];

        let outcome = refused(&github)?;

        assert_eq!(
            unreadable(&outcome),
            (!kept).then_some(StatusUnreadable::Branch),
            "branch {branch:?}"
        );
    }
    Ok(())
}

#[test]
fn a_repository_name_that_is_not_owner_and_name_is_refused() -> Result<(), Box<dyn Error>> {
    let outcome = crate::status::report_named(&FakeGitHub::new(), "not-a-repository")?;

    assert_matches!(outcome, Err(StatusError::Repository));
    Ok(())
}

// --- the checks ---

#[test]
fn two_app_verdict_checks_are_refused() -> Result<(), Box<dyn Error>> {
    let mut github = success_world()?;
    let mut second = github.verdict_mut().clone();
    second.url = run_url(9002);
    github.checks.push(second);

    assert_matches!(
        refused(&github)?,
        Err(StatusError::DuplicateVerdict { count: 2 })
    );
    Ok(())
}

#[test]
fn an_external_id_assay_does_not_write_is_refused() -> Result<(), Box<dyn Error>> {
    for id in [
        None,
        Some("weird"),
        Some("pending:7:"),
        Some("pending:x:3"),
        Some("sha256:abc"),
    ] {
        let mut github = success_world()?;
        github.verdict_mut().external_id = id.map(str::to_owned);

        let outcome = refused(&github)?;

        assert_eq!(
            unreadable(&outcome),
            Some(StatusUnreadable::ExternalId),
            "{id:?}"
        );
    }
    Ok(())
}

#[test]
fn a_check_text_publish_does_not_write_or_naming_another_key_is_refused()
-> Result<(), Box<dyn Error>> {
    let other_key = "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    for text in [None, Some("garbage".to_owned())] {
        let mut github = success_world()?;
        github.verdict_mut().output_text = text;

        assert_eq!(
            unreadable(&refused(&github)?),
            Some(StatusUnreadable::CheckText)
        );
    }
    let mut github = success_world()?;
    github.verdict_mut().external_id = Some(other_key.to_owned());
    assert_eq!(
        unreadable(&refused(&github)?),
        Some(StatusUnreadable::CheckText)
    );
    Ok(())
}

#[test]
fn a_check_whose_status_and_conclusion_disagree_is_refused() -> Result<(), Box<dyn Error>> {
    let cases = [
        (CheckStatus::Completed, None),
        (CheckStatus::InProgress, Some(ObservedConclusion::Success)),
    ];
    for (status, conclusion) in cases {
        let mut github = success_world()?;
        let verdict = github.verdict_mut();
        verdict.status = status;
        verdict.conclusion = conclusion;

        assert_eq!(
            unreadable(&refused(&github)?),
            Some(StatusUnreadable::CheckState),
            "{status:?} {conclusion:?}"
        );
    }
    Ok(())
}

#[test]
fn a_page_url_off_the_repository_is_refused() -> Result<(), Box<dyn Error>> {
    let urls = [
        "https://evil.example/Wet-Ink-Corporation/assay/runs/1",
        "https://github.com/Wet-Ink-Corporation/assay-evil/runs/1",
        "http://github.com/Wet-Ink-Corporation/assay/runs/1",
        "https://github.com/Wet-Ink-Corporation/assay/runs/1 x",
        "https://github.com/Wet-Ink-Corporation/assay/runs/<1>",
    ];
    for url in urls {
        let mut github = success_world()?;
        url.clone_into(&mut github.verdict_mut().url);

        assert_eq!(
            unreadable(&refused(&github)?),
            Some(StatusUnreadable::Url),
            "{url}"
        );
    }
    let mut github = failure_world()?;
    for comment in &mut github.comments {
        "https://evil.example/x".clone_into(&mut comment.url);
    }
    assert_eq!(unreadable(&refused(&github)?), Some(StatusUnreadable::Url));
    Ok(())
}

#[test]
fn a_gate_check_name_that_is_not_a_plugin_or_repeated_is_refused() -> Result<(), Box<dyn Error>> {
    let github = FakeGitHub::new().with_gate("Not A Plugin", ObservedConclusion::Success, 1);
    assert_eq!(
        unreadable(&refused(&github)?),
        Some(StatusUnreadable::GateName)
    );

    let github = FakeGitHub::new()
        .with_gate("temper", ObservedConclusion::Success, 1)
        .with_gate("temper", ObservedConclusion::Failure, 2);
    assert_matches!(
        refused(&github)?,
        Err(StatusError::DuplicateGate { plugin }) if plugin.as_str() == "temper"
    );
    Ok(())
}

#[test]
fn a_gate_check_from_another_app_is_ignored() -> Result<(), Box<dyn Error>> {
    let mut github = FakeGitHub::new();
    github.checks.push(CheckView {
        app: other_app(),
        ..FakeGitHub::new()
            .with_gate("temper", ObservedConclusion::Success, 1)
            .checks
            .remove(0)
    });

    let status = document(&github, &Ask::new())?;

    assert_eq!(status["gates"], Value::Array(Vec::new()));
    Ok(())
}

// --- the bundle ---

#[test]
fn a_bundle_the_store_cannot_give_is_refused() -> Result<(), Box<dyn Error>> {
    let cases = [
        (Stored::Missing, "Missing"),
        (Stored::Expired, "Expired"),
        (Stored::TooLarge, "TooLarge"),
        (Stored::Fails, "Fetch(FakeError)"),
    ];
    for (stored, expected) in cases {
        let mut github = success_world()?;
        github.bundle = stored;

        assert_eq!(bundle_problem(refused(&github)?).as_deref(), Some(expected));
    }
    Ok(())
}

#[test]
fn a_bundle_is_fetched_once_for_the_pointers_run_under_its_cap() -> Result<(), Box<dyn Error>> {
    let github = success_world()?;

    refused(&github)??;

    let fetches = github.fetches.borrow();
    let [(run, file, limit)] = fetches.as_slice() else {
        return Err(format!("expected one fetch, got {fetches:?}").into());
    };
    assert_eq!(
        (
            run.run_id.get(),
            run.run_attempt.get(),
            file.as_str(),
            *limit
        ),
        (7, 1, assay_app::BUNDLE_FILE, assay_app::MAX_BUNDLE_BYTES)
    );
    Ok(())
}

#[test]
fn a_bundle_that_does_not_hash_to_the_pointer_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = success_world()?;
    let Stored::Bytes(bytes) = &mut github.bundle else {
        return Err("a persisted bundle".into());
    };
    bytes.push(b' ');

    assert_eq!(bundle_problem(refused(&github)?).as_deref(), Some("Digest"));
    Ok(())
}

#[test]
fn a_pointer_to_a_rerun_is_refused_before_any_fetch() -> Result<(), Box<dyn Error>> {
    let mut github = success_world()?;
    let verdict = github.verdict_mut();
    let text = verdict
        .output_text
        .as_deref()
        .ok_or("a decided check's text")?;
    verdict.output_text = Some(text.replace("run_attempt 1,", "run_attempt 2,"));

    let outcome = refused(&github)?;

    assert_eq!(bundle_problem(outcome).as_deref(), Some("NotFirstAttempt"));
    assert_eq!(github.fetches.borrow().len(), 0);
    Ok(())
}

#[test]
fn a_bundle_that_is_not_run_inputs_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = success_world()?;
    github.repoint(b"{\"schema\": \"assay.run-inputs/1\"}".to_vec());

    assert_matches!(
        refused(&github)?,
        Err(StatusError::Bundle(BundleProblem::Shape(_)))
    );
    Ok(())
}

#[test]
fn a_bundle_of_another_run_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = success_world()?;
    let mut bundle = github.bundle_value();
    bundle["run_uid"]["run_id"] = Value::from(8);
    github.repoint(serde_json::to_vec(&bundle)?);

    assert_eq!(
        bundle_problem(refused(&github)?).as_deref(),
        Some("Inconsistent")
    );
    Ok(())
}

#[test]
fn a_bundle_whose_decision_is_not_the_checks_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = success_world()?;
    let mut bundle = github.bundle_value();
    bundle["decision"]["decision_key"] =
        Value::from("sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
    github.repoint(serde_json::to_vec(&bundle)?);

    assert_eq!(
        bundle_problem(refused(&github)?).as_deref(),
        Some("Decision")
    );
    Ok(())
}

#[test]
fn a_bundle_decided_under_another_rubric_version_is_refused() -> Result<(), Box<dyn Error>> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rubric/v1.toml");
    let text = std::fs::read_to_string(path)?.replacen("version = \"1\"", "version = \"2\"", 1);
    let rubric: Rubric = toml::from_str(&text)?;

    let outcome = report_with(&success_world()?, &Ask::new(), &rubric)?;

    assert_eq!(bundle_problem(outcome).as_deref(), Some("RubricVersion"));
    Ok(())
}

// --- the viewer and the document ---

#[test]
fn a_viewer_login_that_is_not_a_github_login_is_refused() -> Result<(), Box<dyn Error>> {
    for login in ["", "@owner", "-dash", "a b", &"x".repeat(40)] {
        let github = FakeGitHub::new().viewed_by(3, login);

        assert_eq!(
            unreadable(&refused(&github)?),
            Some(StatusUnreadable::Login),
            "{login:?}"
        );
    }
    let github = FakeGitHub::new().viewed_by(3, "assay-verdict[bot]");
    assert_eq!(
        document(&github, &Ask::new())?["viewer"]["login"].as_str(),
        Some("assay-verdict[bot]")
    );
    Ok(())
}

#[test]
fn a_fetched_at_that_is_not_rfc_3339_fails_the_schema() -> Result<(), Box<dyn Error>> {
    let outcome = crate::status::report_fetched_at(&FakeGitHub::new(), "yesterday")?;

    assert_matches!(outcome, Err(StatusError::Invalid(_)));
    Ok(())
}

// --- the App's comments ---

#[test]
fn only_the_apps_comments_naming_a_decided_finding_are_read() -> Result<(), Box<dyn Error>> {
    let mut github = failure_world()?;
    github.unfiltered = true;
    let forged = COMMENT.replace("Event recorded twice", "Forged title");
    github.comments.insert(
        0,
        ReviewCommentView {
            app: None,
            body: forged.clone(),
            path: "evil.rs".to_owned(),
            line: NonZeroU64::new(1),
            url: run_url(1),
        },
    );
    github.comments.insert(
        0,
        ReviewCommentView {
            app: Some(other_app()),
            body: forged,
            path: "evil.rs".to_owned(),
            line: NonZeroU64::new(1),
            url: run_url(1),
        },
    );
    let github = github
        .with_comment("<!-- assay-summary -->\n\nnot a finding", ("x.rs", 1), 600)
        .with_comment(
            &COMMENT.replace("00000000000000ab", "00000000000000ff"),
            ("y.rs", 1),
            601,
        )
        .with_comment(
            &COMMENT.replace("Event recorded twice", "Second comment"),
            ("z.rs", 1),
            602,
        );

    let status = document(&github, &Ask::new())?;

    let titles: Vec<&str> = status["findings"]
        .as_array()
        .ok_or("findings")?
        .iter()
        .filter_map(|finding| finding["title"].as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "temper: raised",
            "temper: clippy::unwrap_used",
            "Event recorded twice"
        ],
        "the App's first comment per decided fingerprint, nobody else's"
    );
    Ok(())
}

#[test]
fn a_published_body_over_its_cap_is_left_out() -> Result<(), Box<dyn Error>> {
    let mut github = failure_world()?;
    let long = COMMENT.replace(
        "The retry path records the event again\\.",
        &"x".repeat(16_384),
    );
    github.comments = Vec::new();
    let github = github.with_comment(&long, ("src/lib.rs", 12), 501);
    let ask = Ask {
        detail: Detail::Full,
        ..Ask::new()
    };

    let status = document(&github, &ask)?;

    assert_eq!(status.pointer("/findings/2/body"), None);
    assert_eq!(
        status.pointer("/findings/2/title"),
        Some(&Value::from("Event recorded twice"))
    );
    Ok(())
}

// --- caps (H-13), each at the cap and one past it ---

fn filler_check(index: usize) -> CheckView {
    CheckView {
        name: format!("ci / job {index}"),
        ..forged_verdict(ObservedConclusion::Success)
    }
}

#[test]
fn check_runs_at_the_cap_are_read_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = FakeGitHub::new();
    github.checks = (0..MAX_CHECK_RUNS).map(filler_check).collect();
    refused(&github)??;

    github.checks.push(filler_check(MAX_CHECK_RUNS));

    assert_matches!(
        refused(&github)?,
        Err(StatusError::TooManyChecks { count }) if count == MAX_CHECK_RUNS + 1
    );
    Ok(())
}

fn filler_comment(index: usize) -> ReviewCommentView {
    ReviewCommentView {
        app: Some(app()),
        body: format!("comment {index}"),
        path: "src/lib.rs".to_owned(),
        line: None,
        url: run_url(1),
    }
}

#[test]
fn app_comments_at_the_cap_are_read_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let mut github = failure_world()?;
    github.comments = (0..MAX_PRIOR_COMMENTS).map(filler_comment).collect();
    refused(&github)??;

    github.comments.push(filler_comment(MAX_PRIOR_COMMENTS));

    assert_matches!(
        refused(&github)?,
        Err(StatusError::TooManyComments { count }) if count == MAX_PRIOR_COMMENTS + 1
    );
    Ok(())
}

/// The default run with one failing gate per entry of `gates`, each holding that many
/// findings, published.
fn gates_world(gates: &[usize]) -> Result<FakeGitHub, Box<dyn Error>> {
    let mut run = Run::new()?;
    let mut next = 0_u64;
    let mut reports = Vec::with_capacity(gates.len());
    for (index, &count) in gates.iter().enumerate() {
        let mut findings = Vec::with_capacity(count);
        for _ in 0..count {
            next += 1;
            findings.push(GateFinding {
                fp: format!("{next:016x}").parse()?,
                rule: Some("raised".to_owned()),
                severity: Severity::Minor,
            });
        }
        reports.push(GateReport {
            name: format!("gate-{index}"),
            required: true,
            outcome: GateOutcome::Ran {
                conclusion: GateConclusion::Fail,
                steps: assay_domain::StepAttribution::Unknown,
            },
            findings,
        });
    }
    run.inputs.gates = reports;
    let world = stamped()?;
    run.publish(&world)?;
    Ok(FakeGitHub::published(&world))
}

#[test]
fn findings_at_the_cap_are_listed_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let status = document(&gates_world(&[MAX_FINDINGS])?, &Ask::new())?;
    assert_eq!(
        status["findings"].as_array().map(Vec::len),
        Some(MAX_FINDINGS)
    );

    let outcome = refused(&gates_world(&[MAX_FINDINGS, 1])?)?;

    assert_matches!(
        outcome,
        Err(StatusError::TooManyFindings { count }) if count == MAX_FINDINGS + 1
    );
    Ok(())
}

#[test]
fn reasons_at_the_cap_are_listed_and_one_more_is_refused() -> Result<(), Box<dyn Error>> {
    let status = document(&gates_world(&vec![0; MAX_REASONS])?, &Ask::new())?;
    assert_eq!(
        status
            .pointer("/decision/reasons")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(MAX_REASONS)
    );

    let outcome = refused(&gates_world(&vec![0; MAX_REASONS + 1])?)?;

    assert_matches!(
        outcome,
        Err(StatusError::TooManyReasons { count }) if count == MAX_REASONS + 1
    );
    Ok(())
}

#[test]
fn a_fingerprint_carried_twice_is_listed_once_at_its_highest_severity() -> Result<(), Box<dyn Error>>
{
    let mut run = Run::new()?;
    let shared = fp("00000000000000c1")?;
    run.inputs.gates = vec![GateReport {
        name: "temper".to_owned(),
        required: true,
        outcome: GateOutcome::Ran {
            conclusion: GateConclusion::Fail,
            steps: assay_domain::StepAttribution::Unknown,
        },
        findings: vec![
            GateFinding {
                fp: shared,
                rule: Some("raised".to_owned()),
                severity: Severity::Minor,
            },
            GateFinding {
                fp: shared,
                rule: Some("build".to_owned()),
                severity: Severity::Blocker,
            },
        ],
    }];
    let world = stamped()?;
    run.publish(&world)?;

    let status = document(&FakeGitHub::published(&world), &Ask::new())?;

    assert_eq!(
        status["findings"],
        serde_json::json!([{
            "fp": "00000000000000c1",
            "finding_id": "00000000000000c1",
            "severity": "blocker",
            "class": null,
            "title": "temper: raised",
            "file": null,
            "line": null,
            "waivable": false
        }]),
        "the build finding sharing the fingerprint makes it unwaivable (#59 all or nothing)"
    );
    Ok(())
}
