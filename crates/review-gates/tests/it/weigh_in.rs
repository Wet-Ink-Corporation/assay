//! The Weigh-In adapter (04 §9, #372): the pinned `weighin ci-check --json` run in a reading
//! checkout of head, its `weighin.ci-check/1` report read strictly, and 04 §9 rule 1 and the
//! append-only check reported as warnings that never fail the gate.

use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_app::{BuildJob, GateJob, GateJobOutcome, MAX_DOCUMENT_BYTES, Termination};
use review_domain::{GateConclusion, Severity, Side};
use review_gates::{
    CiCheck, GateEnvironment, WeighInError, WeighInReportError, WeighInSettings,
    WeighInSettingsError,
};

use crate::weigh_in_support::{
    BASE, FakeCiCheck, HEAD, PROGRAM, STORE, Seen, TREE, admit, check, environment, findings, item,
    report, run, seen, violation,
};

const NEUTRAL: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Neutral);
const PASS: GateJobOutcome = GateJobOutcome::Concluded(GateConclusion::Pass);
const JOURNALS: &str = ".kb/_ledger/journal";
const BLOCKING: &str = "weigh-in-open-blocking";
const PENDING: &str = "weigh-in-pending-triage";
const REWRITTEN: &str = "weigh-in-journal-rewritten";
const TRIAGED: (Option<&str>, Option<&str>) = (Some("two-way"), Some("judgment"));
const UNTRIAGED: (Option<&str>, Option<&str>) = (None, None);

/// `(rule, .kb/_ledger/journal, 1, severity)`: where item findings sit.
fn at_journals(rule: &str, severity: Severity) -> Seen {
    (rule.to_owned(), JOURNALS.to_owned(), 1, severity)
}

#[test]
fn runs_ci_check_in_the_tree_with_exactly_the_gate_environment() -> Result<(), Box<dyn Error>> {
    let mut ci_check = FakeCiCheck::printing(report(&[], &[]));

    run(&mut ci_check, (false, "weigh-in"), GateJob::Static)??;

    let [spec] = ci_check.seen.as_slice() else {
        return Err(format!("one run, not {:?}", ci_check.seen).into());
    };
    assert_eq!(spec.program, Path::new(PROGRAM));
    assert_eq!(
        spec.args,
        [
            "ci-check", "--json", "--base", BASE, "--dir", TREE, "--store", STORE
        ]
    );
    assert_eq!(spec.cwd, Path::new(TREE));
    assert_eq!(spec.env, environment().vars(), "no token, nothing added");
    assert_eq!(spec.timeout, Duration::from_secs(45));
    assert_eq!(spec.max_output_bytes, MAX_DOCUMENT_BYTES);
    Ok(())
}

#[test]
fn a_report_with_nothing_open_and_clean_journals_passes() -> Result<(), Box<dyn Error>> {
    let seen = seen(report(&[], &[]))?;

    assert_eq!(seen, (PASS, vec![]));
    Ok(())
}

#[test]
fn an_untriaged_open_item_is_a_pending_triage_warning() -> Result<(), Box<dyn Error>> {
    let seen = seen(report(
        &[item("wi-aa4c3b", "raised", UNTRIAGED, "unknown")],
        &[],
    ))?;

    assert_eq!(seen, (NEUTRAL, vec![at_journals(PENDING, Severity::Minor)]));
    Ok(())
}

#[test]
fn a_half_triaged_item_is_pending_triage_not_clear() -> Result<(), Box<dyn Error>> {
    let items = [
        item("wi-aa4c3b", "triaged", (Some("two-way"), None), "none"),
        item("wi-0a1b", "triaged", (None, Some("judgment")), "none"),
    ];

    let seen = seen(report(&items, &[]))?;

    let pending = at_journals(PENDING, Severity::Minor);
    assert_eq!(seen, (NEUTRAL, vec![pending.clone(), pending]));
    Ok(())
}

#[test]
fn a_triaged_one_way_item_is_open_and_blocking() -> Result<(), Box<dyn Error>> {
    let items = [item(
        "wi-aa4c3b",
        "triaged",
        (Some("one-way"), Some("authority")),
        "merge",
    )];

    let (outcome, findings) = findings(report(&items, &[]))?;

    assert_eq!(outcome, NEUTRAL);
    let [finding] = findings.as_slice() else {
        return Err(format!("one finding, not {findings:?}").into());
    };
    assert_eq!(
        (
            finding.rule.as_deref(),
            finding.file.as_str(),
            finding.line.get()
        ),
        (Some(BLOCKING), JOURNALS, 1)
    );
    assert_eq!(finding.severity, Severity::Major);
    assert_eq!(
        finding.title,
        "Weigh-In item `wi-aa4c3b` is open and blocking"
    );
    assert!(
        finding.body.starts_with(
            "`wi-aa4c3b` is triaged, and blocks because its door is `one-way`, not `two-way`, \
             its authority is `authority`, not `judgment`, and it blocks `merge`, not `none` \
             (04 §9 rule 1)"
        ),
        "{}",
        finding.body
    );
    Ok(())
}

#[test]
fn a_triaged_item_blocks_on_its_blocks_door_or_authority_alone() -> Result<(), Box<dyn Error>> {
    let cases = [
        (TRIAGED, "merge", "it blocks `merge`, not `none`"),
        (TRIAGED, "release", "it blocks `release`, not `none`"),
        (TRIAGED, "unknown", "it blocks `unknown`, not `none`"),
        (
            (Some("unsure"), Some("judgment")),
            "none",
            "its door is `unsure`, not `two-way`",
        ),
        (
            (Some("two-way"), Some("authority")),
            "none",
            "its authority is `authority`, not `judgment`",
        ),
    ];
    for (triage, blocks, because) in cases {
        let (_, findings) = findings(report(&[item("wi-aa4c3b", "triaged", triage, blocks)], &[]))?;

        let [finding] = findings.as_slice() else {
            return Err(format!("{triage:?} {blocks}: one finding, not {findings:?}").into());
        };
        assert_eq!(
            finding.rule.as_deref(),
            Some(BLOCKING),
            "{triage:?} {blocks}"
        );
        assert!(
            finding
                .body
                .contains(&format!("blocks because {because} (04 §9 rule 1)")),
            "{triage:?} {blocks}: {}",
            finding.body
        );
    }
    Ok(())
}

#[test]
fn a_two_way_judgment_item_blocking_nothing_is_not_reported() -> Result<(), Box<dyn Error>> {
    let seen = seen(report(
        &[item("wi-aa4c3b", "triaged", TRIAGED, "none")],
        &[],
    ))?;

    assert_eq!(seen, (PASS, vec![]));
    Ok(())
}

#[test]
fn every_open_status_is_reported_with_its_status() -> Result<(), Box<dyn Error>> {
    let statuses = ["raised", "triaged", "grounded", "presented", "deferred"];
    let items: Vec<_> = statuses
        .iter()
        .zip(["wi-0001", "wi-0002", "wi-0003", "wi-0004", "wi-0005"])
        .map(|(status, id)| item(id, status, UNTRIAGED, "unknown"))
        .collect();

    let (_, findings) = findings(report(&items, &[]))?;

    let said: Vec<bool> = findings
        .iter()
        .zip(statuses)
        .map(|(finding, status)| finding.body.contains(&format!(" is {status} and")))
        .collect();
    assert_eq!(said, [true; 5], "{findings:?}");
    Ok(())
}

#[test]
fn each_journal_violation_is_a_rewritten_journal_warning_at_its_line() -> Result<(), Box<dyn Error>>
{
    let violations = [
        violation(".kb/_ledger/journal/s1.jsonl", Some(3), "missing"),
        violation(".kb/_ledger/journal/s2.jsonl", None, "removed"),
        violation(".kb/_ledger/journal/s3.jsonl", Some(7), "altered"),
        violation(".kb/_ledger/journal/s4.jsonl", Some(2), "undecodable"),
    ];
    let items = [item("wi-aa4c3b", "raised", UNTRIAGED, "unknown")];

    let seen = seen(report(&items, &violations))?;
    let (_, findings) = findings(report(&items, &violations))?;

    let sides: Vec<Option<Side>> = findings.iter().map(|f| f.side).collect();
    assert_eq!(
        sides,
        [
            Some(Side::Left),
            Some(Side::Left),
            Some(Side::Left),
            Some(Side::Right),
            Some(Side::Right),
        ],
        "missing, removed and altered at base's line; undecodable and items at head"
    );
    let rewritten =
        |file: &str, line| (REWRITTEN.to_owned(), file.to_owned(), line, Severity::Major);
    assert_eq!(
        seen,
        (
            NEUTRAL,
            vec![
                rewritten(".kb/_ledger/journal/s1.jsonl", 3),
                rewritten(".kb/_ledger/journal/s2.jsonl", 1),
                rewritten(".kb/_ledger/journal/s3.jsonl", 7),
                rewritten(".kb/_ledger/journal/s4.jsonl", 2),
                at_journals(PENDING, Severity::Minor),
            ]
        ),
        "violations first, a removed journal at line 1"
    );
    Ok(())
}

#[test]
fn a_violation_path_cannot_break_out_of_its_code_span() -> Result<(), Box<dyn Error>> {
    let violations = [violation(
        ".kb/x`\n[l](https://e).jsonl",
        Some(1),
        "missing",
    )];

    let (_, findings) = findings(report(&[], &violations))?;

    let texts: Vec<(&str, &str)> = findings
        .iter()
        .map(|f| (f.title.as_str(), f.body.as_str()))
        .collect();
    assert!(
        texts.len() == 1
            && texts.iter().all(
                |(title, body)| title.contains("`.kb/x???l??https?e?.jsonl`")
                    && !body.contains("https://")
            ),
        "{texts:?}"
    );
    Ok(())
}

#[test]
fn the_question_and_deadline_are_never_quoted() -> Result<(), Box<dyn Error>> {
    let items = [
        item("wi-aa4c3b", "raised", UNTRIAGED, "unknown"),
        item(
            "wi-0a1b",
            "triaged",
            (Some("one-way"), Some("judgment")),
            "merge",
        ),
    ];

    let (_, findings) = findings(report(&items, &[]))?;

    assert_eq!(findings.len(), 2);
    for finding in findings {
        let text = [finding.evidence, finding.title, finding.body].concat();
        assert!(!text.contains("SECRET"), "{text}");
    }
    Ok(())
}

#[test]
fn every_warning_says_the_gate_is_experimental_and_g2_is_enforced_by_cp1()
-> Result<(), Box<dyn Error>> {
    let items = [
        item("wi-aa4c3b", "raised", UNTRIAGED, "unknown"),
        item(
            "wi-0a1b",
            "triaged",
            (Some("one-way"), Some("authority")),
            "merge",
        ),
    ];
    let violations = [violation(
        ".kb/_ledger/journal/s1.jsonl",
        Some(1),
        "missing",
    )];

    let (_, findings) = findings(report(&items, &violations))?;

    assert_eq!(findings.len(), 3);
    for finding in findings {
        assert!(
            finding
                .body
                .contains("this gate is experimental and never fails")
                && finding
                    .body
                    .contains("G2 is enforced by CP1 (`assay check --local`)"),
            "{}",
            finding.body
        );
    }
    Ok(())
}

#[test]
fn more_than_a_thousand_findings_end_with_one_truncation_note() -> Result<(), Box<dyn Error>> {
    let items: Vec<_> = (0..1_005)
        .map(|index| item(&format!("wi-{index:04x}"), "raised", UNTRIAGED, "unknown"))
        .collect();

    let (outcome, findings) = findings(report(&items, &[]))?;

    let rules: Vec<&str> = findings.iter().filter_map(|f| f.rule.as_deref()).collect();
    assert_eq!(outcome, NEUTRAL);
    assert_eq!(rules.len(), 1_001);
    assert_eq!(rules.iter().filter(|rule| **rule == PENDING).count(), 1_000);
    assert_eq!(rules.last(), Some(&"weigh-in-truncated"));
    assert!(
        findings
            .last()
            .is_some_and(|last| last.body.starts_with("5 more Weigh-In findings")),
        "{:?}",
        findings.last()
    );
    Ok(())
}

#[test]
fn a_run_that_does_not_exit_zero_is_an_error_never_a_pass() -> Result<(), Box<dyn Error>> {
    let ended = [
        Termination::Exited(1),
        Termination::Exited(2),
        Termination::Signalled,
        Termination::TimedOut,
        Termination::OutputTooLarge,
    ];
    for termination in ended {
        let mut ci_check = FakeCiCheck {
            termination,
            ..FakeCiCheck::printing(report(&[], &[]))
        };

        let ran = run(&mut ci_check, (false, "weigh-in"), GateJob::Static)?;

        assert!(
            matches!(ran, Err(WeighInError::Ended { termination: ended }) if ended == termination),
            "{termination:?}: {ran:?}"
        );
    }
    Ok(())
}

#[test]
fn a_supervisor_failure_is_an_error() -> Result<(), Box<dyn Error>> {
    let mut ci_check = FakeCiCheck {
        fails: true,
        ..FakeCiCheck::printing(Vec::new())
    };

    let ran = run(&mut ci_check, (false, "weigh-in"), GateJob::Static)?;

    assert!(matches!(ran, Err(WeighInError::Supervisor(_))), "{ran:?}");
    Ok(())
}

#[test]
fn a_refused_report_is_an_error_never_a_partial_read() -> Result<(), Box<dyn Error>> {
    let mut other = FakeCiCheck::printing(
        String::from_utf8(report(&[], &[]))?
            .replace("weighin.ci-check/1", "weighin.ci-check/2")
            .into_bytes(),
    );
    let mut wrong_head = FakeCiCheck::printing(
        String::from_utf8(report(&[], &[]))?
            .replace(
                &format!("\"head\":\"{HEAD}\""),
                &format!("\"head\":\"{BASE}\""),
            )
            .into_bytes(),
    );

    let other = run(&mut other, (false, "weigh-in"), GateJob::Static)?;
    let wrong_head = run(&mut wrong_head, (false, "weigh-in"), GateJob::Static)?;

    assert!(
        matches!(other, Err(WeighInError::Report(WeighInReportError::Schema))),
        "{other:?}"
    );
    assert!(
        matches!(
            wrong_head,
            Err(WeighInError::Report(WeighInReportError::Head))
        ),
        "{wrong_head:?}"
    );
    Ok(())
}

#[test]
fn admit_makes_runs_refusals_and_admits_a_non_required_gate() -> Result<(), Box<dyn Error>> {
    let lint = GateJob::Build(BuildJob::Lint);

    let admitted = admit((false, "weigh-in"), GateJob::Static)?;
    let required = admit((true, "weigh-in"), GateJob::Static)?;
    let temper = admit((false, "temper"), GateJob::Static)?;
    let outside = admit((false, "weigh-in"), lint)?;

    assert!(admitted.is_ok(), "{admitted:?}");
    assert!(
        matches!(required, Err(WeighInError::Required { ref plugin }) if plugin == "weigh-in"),
        "{required:?}"
    );
    assert!(
        matches!(temper, Err(WeighInError::NotWeighIn { ref plugin }) if plugin == "temper"),
        "{temper:?}"
    );
    assert!(
        matches!(outside, Err(WeighInError::OutsideStatic { job }) if job == lint),
        "{outside:?}"
    );
    Ok(())
}

#[test]
fn refuses_a_required_weigh_in_plugin_before_running() -> Result<(), Box<dyn Error>> {
    let mut ci_check = FakeCiCheck::printing(report(&[], &[]));

    let ran = run(&mut ci_check, (true, "weigh-in"), GateJob::Static)?;

    assert!(
        matches!(ran, Err(WeighInError::Required { ref plugin }) if plugin == "weigh-in"),
        "{ran:?}"
    );
    assert!(ci_check.seen.is_empty(), "nothing ran");
    Ok(())
}

#[test]
fn refuses_another_adapter_and_any_job_but_gates_static() -> Result<(), Box<dyn Error>> {
    let mut ci_check = FakeCiCheck::printing(report(&[], &[]));

    let temper = run(&mut ci_check, (false, "temper"), GateJob::Static)?;
    let lint = run(
        &mut ci_check,
        (false, "weigh-in"),
        GateJob::Build(BuildJob::Lint),
    )?;
    let test = run(
        &mut ci_check,
        (false, "weigh-in"),
        GateJob::Build(BuildJob::Test),
    )?;

    assert!(
        matches!(temper, Err(WeighInError::NotWeighIn { ref plugin }) if plugin == "temper"),
        "{temper:?}"
    );
    assert!(
        matches!(
            lint,
            Err(WeighInError::OutsideStatic {
                job: GateJob::Build(BuildJob::Lint)
            })
        ),
        "{lint:?}"
    );
    assert!(
        matches!(
            test,
            Err(WeighInError::OutsideStatic {
                job: GateJob::Build(BuildJob::Test)
            })
        ),
        "{test:?}"
    );
    assert!(ci_check.seen.is_empty(), "nothing ran");
    Ok(())
}

#[test]
fn refuses_an_empty_run_key() -> Result<(), Box<dyn Error>> {
    let settings = WeighInSettings::new(check()?, &environment(), "");

    assert!(
        matches!(settings, Err(WeighInSettingsError::EmptyRunKey)),
        "{settings:?}"
    );
    Ok(())
}

#[test]
fn refuses_a_relative_program_tree_or_store() -> Result<(), Box<dyn Error>> {
    let relative = PathBuf::from("weighin");
    let program = CiCheck {
        program: relative.clone(),
        ..check()?
    };
    let tree = CiCheck {
        tree: relative.clone(),
        ..check()?
    };
    let store = CiCheck {
        store: relative.clone(),
        ..check()?
    };

    let refused = [program, tree, store]
        .map(|check| WeighInSettings::new(check, &environment(), "run-1").err());

    assert_eq!(
        refused,
        [
            Some(WeighInSettingsError::RelativeProgram {
                path: relative.clone()
            }),
            Some(WeighInSettingsError::RelativeTree {
                path: relative.clone()
            }),
            Some(WeighInSettingsError::RelativeStore { path: relative }),
        ]
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn refuses_a_tree_or_store_that_is_not_utf8_naming_that_path() -> Result<(), Box<dyn Error>> {
    use std::os::unix::ffi::OsStrExt;

    let not_utf8 = |name: &[u8]| Path::new(std::ffi::OsStr::from_bytes(name)).to_path_buf();
    let bad_tree = not_utf8(b"/scratch/weigh-in-\xff");
    let bad_store = not_utf8(b"/scratch/store-\xfe/ledger.jsonl");
    let tree = CiCheck {
        tree: bad_tree.clone(),
        ..check()?
    };
    let store = CiCheck {
        store: bad_store.clone(),
        ..check()?
    };

    let refused =
        [tree, store].map(|check| WeighInSettings::new(check, &environment(), "run-1").err());

    assert_eq!(
        refused,
        [
            Some(WeighInSettingsError::NotUtf8 { path: bad_tree }),
            Some(WeighInSettingsError::NotUtf8 { path: bad_store }),
        ]
    );
    Ok(())
}

#[test]
fn refuses_a_search_path_with_a_relative_entry() -> Result<(), Box<dyn Error>> {
    for path in ["/usr/bin::/bin", "bin:/usr/bin", "/usr/bin:."] {
        let environment =
            GateEnvironment::from_vars([(OsString::from("PATH"), OsString::from(path))]);

        let settings = WeighInSettings::new(check()?, &environment, "run-1");

        assert!(
            matches!(settings, Err(WeighInSettingsError::RelativeSearchPath)),
            "{path}: {settings:?}"
        );
    }
    Ok(())
}
