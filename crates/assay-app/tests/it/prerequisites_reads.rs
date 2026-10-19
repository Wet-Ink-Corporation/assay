//! doctor's repository checks read what several of them share once per run (#75 review round
//! 1, F7): the rulesets and the workflow files. A shared read that fails reports its cause
//! under the first check that needs it and fails the others as already reported.

use assay_app::{Prerequisite, PrerequisiteFailure, PrerequisiteReport, prerequisites, read_base};

use crate::plan_fakes::FakeError;
use crate::prerequisites_fakes::{FakeRepository, apps};

type Failure = PrerequisiteFailure<FakeError>;

/// The report for `repository` and `ledger`, with base config read from `repository`.
fn report(repository: &FakeRepository, ledger: &FakeRepository) -> PrerequisiteReport<FakeError> {
    let base = read_base(repository).expect("base config is readable");
    prerequisites(repository, Some(ledger), &base, apps())
}

/// Each failed prerequisite with its failure, in report order.
fn failures(report: &PrerequisiteReport<FakeError>) -> Vec<(Prerequisite, &Failure)> {
    report
        .results()
        .iter()
        .filter_map(|result| Some((result.prerequisite, result.failure.as_ref()?)))
        .collect()
}

#[test]
fn the_rulesets_and_each_workflow_file_are_read_once() {
    let repository = FakeRepository::healthy();
    let ledger = FakeRepository::healthy_ledger();

    let report = report(&repository, &ledger);

    assert!(report.passed(), "{report:#?}");
    assert_eq!(repository.reads_of("rulesets"), 1);
    assert_eq!(repository.reads_of("directory"), 1);
    for name in ["assay.yml", "assay-decide.yml", "rust.yml"] {
        assert_eq!(
            repository.reads_of(&format!("file:.github/workflows/{name}")),
            1,
            "{name}"
        );
    }
    assert_eq!(ledger.reads_of("rulesets"), 1);
}

#[test]
fn a_shared_read_that_fails_reports_its_cause_once() {
    let mut repository = FakeRepository::healthy();
    repository.broken.insert("directory");
    repository.broken.insert("rulesets");
    let ledger = FakeRepository::healthy_ledger();

    let report = report(&repository, &ledger);
    let failures = failures(&report);

    let failed: Vec<_> = failures
        .iter()
        .map(|(prerequisite, _)| *prerequisite)
        .collect();
    assert_eq!(
        failed,
        vec![
            Prerequisite::VerdictRuleset,
            Prerequisite::MergeCommitTrigger,
            Prerequisite::PathRestriction,
            Prerequisite::PinnedActions,
            Prerequisite::RunAttemptGuard,
        ]
    );
    let unreadable: Vec<_> = failures
        .iter()
        .filter(|(_, failure)| matches!(failure, Failure::Unreadable(FakeError)))
        .map(|(prerequisite, _)| *prerequisite)
        .collect();
    assert_eq!(
        unreadable,
        vec![
            Prerequisite::VerdictRuleset,
            Prerequisite::MergeCommitTrigger
        ]
    );
    for (prerequisite, failure) in &failures {
        if !unreadable.contains(prerequisite) {
            assert!(
                matches!(failure, Failure::AlreadyReported { .. }),
                "{prerequisite}: {failure:?}"
            );
        }
    }
}
