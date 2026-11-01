//! Run provenance (J4, F19, `docs/architecture/02-architecture.md` §5): a bundle pointer is
//! believed only for attempt 1 of the expected workflow file of this repository, under the
//! events that workflow runs for, on this repository's head and, for a run started on a branch,
//! on the default branch. Each refusal is pinned by changing one field of an accepted run.

use std::num::NonZeroU64;

use assay_app::{ExpectedRun, ProvenanceRefusal, RunEvent, Workflow, WorkflowRunFacts, accept_run};
use assay_domain::RunUid;

/// The repository the runs are of.
const REPO: u64 = 1_413_773_798;
/// The run the pointer names.
const RUN: u64 = 41_260_608_626;

fn id(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("not zero")
}

/// The run the pointer names: attempt 1 of `REPO`'s run `RUN`.
fn pointed() -> RunUid {
    RunUid {
        repo_id: id(REPO),
        run_id: id(RUN),
        run_attempt: id(1),
    }
}

/// What the pipeline expects of the run.
fn pipeline() -> ExpectedRun<'static> {
    ExpectedRun {
        workflow: Workflow::Pipeline,
        run: pointed(),
        default_branch: "main",
    }
}

/// The pipeline's `pull_request_target` run of a same-repository PR's branch.
fn pull_request_target() -> WorkflowRunFacts {
    WorkflowRunFacts {
        run_id: id(RUN),
        repo_id: id(REPO),
        path: ".github/workflows/assay.yml".to_owned(),
        event: RunEvent::PullRequestTarget,
        head_branch: "claude/assay-60-st8-gates-lint".to_owned(),
        head_repo: Some(id(REPO)),
        run_attempt: id(1),
    }
}

/// The pipeline's `workflow_dispatch` run (a requeue) on the default branch.
fn dispatch() -> WorkflowRunFacts {
    WorkflowRunFacts {
        event: RunEvent::WorkflowDispatch,
        head_branch: "main".to_owned(),
        ..pull_request_target()
    }
}

#[test]
fn a_pull_request_target_run_of_the_pipeline_is_accepted() {
    assert_eq!(accept_run(&pull_request_target(), &pipeline()), Ok(()));
}

#[test]
fn a_dispatch_of_the_pipeline_on_the_default_branch_is_accepted() {
    assert_eq!(accept_run(&dispatch(), &pipeline()), Ok(()));
}

/// A case: its name, the accepted run it changes, the one change, and the refusal.
type Refused = (
    &'static str,
    fn() -> WorkflowRunFacts,
    fn(&mut WorkflowRunFacts),
    ProvenanceRefusal,
);

/// Asserts that each case's accepted run is accepted, and refused once changed.
fn each_refused(cases: &[Refused]) {
    for &(case, accepted, change, refusal) in cases {
        let mut facts = accepted();
        assert_eq!(accept_run(&facts, &pipeline()), Ok(()), "{case}: accepted");
        change(&mut facts);

        assert_eq!(accept_run(&facts, &pipeline()), Err(refusal), "{case}");
    }
}

#[test]
fn another_run_repository_or_workflow_file_is_refused_by_its_own_code() {
    let prt: fn() -> WorkflowRunFacts = pull_request_target;
    each_refused(&[
        (
            "another run",
            prt,
            |f| f.run_id = id(RUN + 1),
            ProvenanceRefusal::OtherRun,
        ),
        (
            "another repository",
            prt,
            |f| f.repo_id = id(REPO + 1),
            ProvenanceRefusal::OtherRepository,
        ),
        (
            "a branch's own workflow",
            prt,
            |f| ".github/workflows/rust.yml".clone_into(&mut f.path),
            ProvenanceRefusal::Workflow,
        ),
        (
            "the workflow at a ref",
            prt,
            |f| f.path.push_str("@refs/heads/main"),
            ProvenanceRefusal::Workflow,
        ),
        (
            "the decide-only workflow",
            prt,
            |f| ".github/workflows/assay-decide.yml".clone_into(&mut f.path),
            ProvenanceRefusal::Workflow,
        ),
    ]);
}

#[test]
fn another_event_branch_head_repository_or_attempt_is_refused_by_its_own_code() {
    let prt: fn() -> WorkflowRunFacts = pull_request_target;
    each_refused(&[
        (
            "a pull_request run",
            prt,
            |f| f.event = RunEvent::Other,
            ProvenanceRefusal::Event,
        ),
        (
            "a comment run",
            dispatch,
            |f| f.event = RunEvent::IssueComment,
            ProvenanceRefusal::Event,
        ),
        (
            "a dispatch on a branch",
            dispatch,
            |f| "feature".clone_into(&mut f.head_branch),
            ProvenanceRefusal::Branch,
        ),
        (
            "a fork's head",
            prt,
            |f| f.head_repo = Some(id(REPO + 1)),
            ProvenanceRefusal::HeadRepository,
        ),
        (
            "a deleted head repository",
            prt,
            |f| f.head_repo = None,
            ProvenanceRefusal::HeadRepository,
        ),
        (
            "a re-run",
            prt,
            |f| f.run_attempt = id(2),
            ProvenanceRefusal::Attempt,
        ),
    ]);
}

#[test]
fn a_pointer_to_a_re_run_is_refused_whatever_the_run_reports() {
    let expected = ExpectedRun {
        run: RunUid {
            run_attempt: id(2),
            ..pointed()
        },
        ..pipeline()
    };

    assert_eq!(
        accept_run(&pull_request_target(), &expected),
        Err(ProvenanceRefusal::Attempt)
    );
}

#[test]
fn the_decide_only_workflow_is_accepted_for_its_own_events_on_the_default_branch() {
    let decide_only = ExpectedRun {
        workflow: Workflow::DecideOnly,
        ..pipeline()
    };
    let at = |event| WorkflowRunFacts {
        path: ".github/workflows/assay-decide.yml".to_owned(),
        event,
        ..dispatch()
    };

    assert_eq!(
        accept_run(&at(RunEvent::IssueComment), &decide_only),
        Ok(())
    );
    assert_eq!(accept_run(&at(RunEvent::Schedule), &decide_only), Ok(()));
    assert_eq!(
        accept_run(&at(RunEvent::PullRequestTarget), &decide_only),
        Ok(())
    );
    assert_eq!(
        accept_run(&at(RunEvent::WorkflowDispatch), &decide_only),
        Err(ProvenanceRefusal::Event)
    );
    assert_eq!(
        accept_run(&dispatch(), &decide_only),
        Err(ProvenanceRefusal::Workflow)
    );
    let on_a_branch = WorkflowRunFacts {
        head_branch: "feature".to_owned(),
        ..at(RunEvent::Schedule)
    };
    assert_eq!(
        accept_run(&on_a_branch, &decide_only),
        Err(ProvenanceRefusal::Branch)
    );
}

#[test]
fn workflow_paths_are_the_installed_files() {
    assert_eq!(Workflow::Pipeline.path(), ".github/workflows/assay.yml");
    assert_eq!(
        Workflow::DecideOnly.path(),
        ".github/workflows/assay-decide.yml"
    );
}
