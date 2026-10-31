//! `publish_findings` over the stateful fake: inline comments posted once per fingerprint,
//! per-gate checks, and the lock and stamp rules they are written under (04 §6, R3-09).

use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    CheckConclusion, CheckRunId, CheckState, CheckStatus, CommentStep, FindingRef, FindingsError,
    FindingsPublication, FindingsPublished, FindingsWritten, MAX_PRIOR_COMMENTS, Placement,
    PullRequestEvent, Target, Unlock, ValidFinding, publish_findings,
};
use assay_domain::{
    Class, Finding, FindingKind, Fingerprint, GateConclusion, GateFinding, GateOutcome, GateReport,
    Severity, Side, StepAttribution,
};

use crate::plan_fakes::{BASE, FakeError, HEAD, run_uid};
use crate::publish_fakes::{Author, Call, FakeCheck, FakeReviewComment, FakeWorld, lease_deadline};
use crate::publish_support::{DEFAULT_BRANCH, KEY, NAME};

type Outcome = Result<FindingsPublished<FakeError>, FindingsError<FakeError, FakeError>>;

/// The gate check's name for the `temper` plugin.
const GATE: &str = "assay / gate: temper";

fn pull_request(repo: &str) -> Result<PullRequestEvent, Box<dyn Error>> {
    Ok(PullRequestEvent {
        repo: repo.to_owned(),
        number: NonZeroU64::new(42).ok_or("not zero")?,
        base_sha: BASE.parse()?,
        head_sha: HEAD.parse()?,
        base_ref: "main".to_owned(),
        draft: false,
    })
}

fn fp(value: u64) -> Result<Fingerprint, Box<dyn Error>> {
    Ok(format!("{value:016x}").parse()?)
}

/// An inline major on `src/lib.rs` line `line` with fingerprint `value`.
fn finding(value: u64, line: u64) -> Result<ValidFinding, Box<dyn Error>> {
    Ok(ValidFinding {
        at: FindingRef::Finding {
            document: 0,
            index: 0,
        },
        finding: Finding {
            class: Class::Logic,
            rule: None,
            kind: FindingKind::Defect,
            severity: Severity::Major,
            introduced_by: None,
            pre_existing: false,
            file: "src/lib.rs".to_owned(),
            line: NonZeroU64::new(line).ok_or("not zero")?,
            end_line: None,
            side: Some(Side::Right),
            evidence: "record_event(id);".to_owned(),
            expected_behavior: None,
            trigger: Some("a retry after a timeout".to_owned()),
            caller: None,
            title: "Event recorded twice".to_owned(),
            body: "See @owner and [this](https://evil.example).".to_owned(),
            fix: None,
            confidence: None,
            group_hint: None,
        },
        file_at_head: "src/lib.rs".to_owned(),
        enclosing_symbol: "f".to_owned(),
        occurrence_index: 0,
        fingerprint: fp(value)?,
        placement: Placement::Inline,
        previously_dismissed: None,
    })
}

fn gate(name: &str, outcome: GateOutcome) -> Result<GateReport, Box<dyn Error>> {
    Ok(GateReport {
        name: name.to_owned(),
        required: true,
        outcome,
        findings: vec![GateFinding {
            fp: fp(0xfeed)?,
            rule: Some("clippy::unwrap_used".to_owned()),
            severity: Severity::Major,
        }],
    })
}

fn failed() -> GateOutcome {
    GateOutcome::Ran {
        conclusion: GateConclusion::Fail,
        steps: StepAttribution::Unknown,
    }
}

/// One run's inputs to `publish_findings`.
struct Inputs {
    pull_request: PullRequestEvent,
    decision_key: String,
    gates: Vec<GateReport>,
    findings: Vec<ValidFinding>,
}

impl Inputs {
    /// Two inline findings and one failed `temper` gate, decided under [`KEY`].
    fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            pull_request: pull_request("owner/repo")?,
            decision_key: KEY.to_owned(),
            gates: vec![gate("temper", failed())?],
            findings: vec![finding(1, 3)?, finding(2, 5)?],
        })
    }

    fn publish(&self, world: &FakeWorld) -> Result<Outcome, Box<dyn Error>> {
        let publication = FindingsPublication {
            target: Target {
                run_uid: run_uid(1)?,
                pull_request: &self.pull_request,
                default_branch: DEFAULT_BRANCH,
            },
            decision_key: &self.decision_key,
            gates: &self.gates,
            findings: &self.findings,
        };
        Ok(publish_findings(&publication, world, world))
    }
}

/// A world whose verdict check carries this run's decision, as `publish` leaves it.
fn decided_world() -> Result<FakeWorld, Box<dyn Error>> {
    FakeWorld::with_check(HEAD, NAME, KEY)
}

fn written(outcome: Outcome) -> Result<FindingsWritten<FakeError>, Box<dyn Error>> {
    match outcome {
        Ok(FindingsPublished::Written(written)) => Ok(written),
        other => Err(format!("expected Written, got {other:?}").into()),
    }
}

#[test]
fn posts_one_inline_comment_per_selected_finding() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;

    let written = written(Inputs::new()?.publish(&world)?)?;

    assert_eq!(written.posted, [fp(1)?, fp(2)?]);
    assert!(written.already_posted.is_empty());
    assert!(matches!(written.unlock, Unlock::Released));
    let comments = world.review_comments.borrow();
    let anchors: Vec<_> = comments
        .iter()
        .map(|comment| {
            (
                comment.author,
                comment.commit_id.as_str(),
                comment.path.as_str(),
                comment.line.get(),
                comment.start_line,
                comment.side,
            )
        })
        .collect();
    assert_eq!(
        anchors,
        [
            (Author::App, HEAD, "src/lib.rs", 3, None, Side::Right),
            (Author::App, HEAD, "src/lib.rs", 5, None, Side::Right)
        ]
    );
    Ok(())
}

#[test]
fn posted_text_is_inert_and_carries_the_marker() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;

    written(Inputs::new()?.publish(&world)?)?;

    let comments = world.review_comments.borrow();
    let body = &comments.first().ok_or("a comment")?.body;
    assert!(
        body.starts_with("<!-- assay-finding 0000000000000001 -->\n"),
        "{body}"
    );
    assert!(
        body.contains(
            "See \u{ff20}owner and \\[this\\]\\(https\\[\\:\\]\\/\\/evil\\.example\\)\\."
        ),
        "{body}"
    );
    Ok(())
}

#[test]
fn a_multi_line_finding_spans_its_lines() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    let mut inputs = Inputs::new()?;
    let mut spanning = finding(1, 3)?;
    spanning.finding.end_line = NonZeroU64::new(6);
    inputs.findings = vec![spanning];

    written(inputs.publish(&world)?)?;

    let comments = world.review_comments.borrow();
    let comment = comments.first().ok_or("a comment")?;
    assert_eq!(
        (comment.start_line, comment.line.get()),
        (NonZeroU64::new(3), 6)
    );
    Ok(())
}

#[test]
fn a_rerun_posts_no_duplicate() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    let inputs = Inputs::new()?;
    written(inputs.publish(&world)?)?;

    let again = written(inputs.publish(&world)?)?;

    assert!(again.posted.is_empty());
    assert_eq!(again.already_posted, [fp(1)?, fp(2)?]);
    assert_eq!(world.review_comments.borrow().len(), 2);
    Ok(())
}

#[test]
fn a_fingerprint_an_earlier_run_posted_is_skipped() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    world
        .review_comments
        .borrow_mut()
        .push(FakeReviewComment::on_head(
            Author::App,
            HEAD,
            "<!-- assay-finding 0000000000000001 -->\nan older body",
        ));

    let written = written(Inputs::new()?.publish(&world)?)?;

    assert_eq!(
        (written.posted, written.already_posted),
        (vec![fp(2)?], vec![fp(1)?])
    );
    Ok(())
}

#[test]
fn yields_when_the_verdict_check_carries_another_decision() -> Result<(), Box<dyn Error>> {
    let world = FakeWorld::with_check(HEAD, NAME, "pending:8:1")?;

    let outcome = Inputs::new()?.publish(&world)?;

    assert!(
        matches!(
            outcome,
            Ok(FindingsPublished::Yielded {
                unlock: Unlock::Released
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(world.writes(), [Call::Acquire, Call::Release]);
    Ok(())
}

#[test]
fn yields_when_there_is_no_verdict_check() -> Result<(), Box<dyn Error>> {
    let world = FakeWorld::default();

    let outcome = Inputs::new()?.publish(&world)?;

    assert!(
        matches!(outcome, Ok(FindingsPublished::Yielded { .. })),
        "{outcome:?}"
    );
    assert_eq!(world.writes(), [Call::Acquire, Call::Release]);
    Ok(())
}

#[test]
fn a_held_lock_publishes_nothing() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    *world.lock.borrow_mut() = Some(99);

    let outcome = Inputs::new()?.publish(&world)?;

    assert!(
        matches!(outcome, Err(FindingsError::LockBusy)),
        "{outcome:?}"
    );
    assert_eq!(world.writes(), [Call::Acquire]);
    Ok(())
}

#[test]
fn a_lock_error_publishes_nothing() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    world.fail(Call::Acquire, 1);

    let outcome = Inputs::new()?.publish(&world)?;

    assert!(
        matches!(outcome, Err(FindingsError::Lock(FakeError))),
        "{outcome:?}"
    );
    assert!(world.review_comments.borrow().is_empty());
    Ok(())
}

#[test]
fn a_failed_post_names_its_step_and_releases_the_lock() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    world.fail(Call::CreateReviewComment, 2);

    let outcome = Inputs::new()?.publish(&world)?;

    assert!(
        matches!(
            outcome,
            Err(FindingsError::Locked {
                step: CommentStep::PostComment,
                source: FakeError,
                unlock: Unlock::Released
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(world.lock.borrow().as_ref(), None);
    Ok(())
}

#[test]
fn a_failed_release_is_reported_beside_the_outcome() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    world.fail(Call::Release, 1);

    let written = written(Inputs::new()?.publish(&world)?)?;

    assert!(matches!(written.unlock, Unlock::Failed(FakeError)));
    assert_eq!(written.posted.len(), 2);
    Ok(())
}

fn with_prior_comments(count: usize) -> Result<FakeWorld, Box<dyn Error>> {
    let world = decided_world()?;
    world
        .review_comments
        .borrow_mut()
        .extend((0..count).map(|_| FakeReviewComment::on_head(Author::App, HEAD, "no marker")));
    Ok(world)
}

#[test]
fn prior_comments_up_to_the_cap_are_read() -> Result<(), Box<dyn Error>> {
    let world = with_prior_comments(MAX_PRIOR_COMMENTS)?;

    let written = written(Inputs::new()?.publish(&world)?)?;

    assert_eq!(written.posted.len(), 2);
    Ok(())
}

#[test]
fn prior_comments_past_the_cap_refuse_before_any_write() -> Result<(), Box<dyn Error>> {
    let world = with_prior_comments(MAX_PRIOR_COMMENTS + 1)?;

    let outcome = Inputs::new()?.publish(&world)?;

    assert!(
        matches!(
            outcome,
            Err(FindingsError::TooManyComments {
                count,
                unlock: Unlock::Released
            }) if count == MAX_PRIOR_COMMENTS + 1
        ),
        "{outcome:?}"
    );
    assert_eq!(world.writes(), [Call::Acquire, Call::Release]);
    Ok(())
}

#[test]
fn a_decision_key_that_is_not_a_digest_is_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    let mut inputs = Inputs::new()?;
    inputs.decision_key = "pending:7:3".to_owned();

    let outcome = inputs.publish(&world)?;

    assert!(
        matches!(outcome, Err(FindingsError::DecisionKey)),
        "{outcome:?}"
    );
    assert!(world.calls.borrow().is_empty());
    Ok(())
}

#[test]
fn a_malformed_repository_is_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    let mut inputs = Inputs::new()?;
    inputs.pull_request = pull_request("owner/repo)[x](https://evil")?;

    let outcome = inputs.publish(&world)?;

    assert!(
        matches!(outcome, Err(FindingsError::Repository)),
        "{outcome:?}"
    );
    assert!(world.calls.borrow().is_empty());
    Ok(())
}

#[test]
fn a_gate_name_that_is_not_a_plugin_name_is_refused_before_any_call() -> Result<(), Box<dyn Error>>
{
    let world = decided_world()?;
    let mut inputs = Inputs::new()?;
    inputs.gates = vec![gate("Temper\n", failed())?];

    let outcome = inputs.publish(&world)?;

    assert!(
        matches!(&outcome, Err(FindingsError::GateName { name }) if name == "Temper\n"),
        "{outcome:?}"
    );
    assert!(world.calls.borrow().is_empty());
    Ok(())
}

#[test]
fn each_gate_gets_its_own_completed_check() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;

    written(Inputs::new()?.publish(&world)?)?;

    let check = world.app_check(GATE)?;
    let output = check.output.ok_or("the gate check was written")?;
    assert_eq!(check.head, HEAD);
    assert_eq!(
        (
            output.state,
            output.external_id.as_str(),
            output.title.as_str()
        ),
        (
            CheckState::Completed(CheckConclusion::Failure),
            KEY,
            "assay gate temper: fail"
        )
    );
    assert_eq!(
        output.summary,
        "Findings: 0 blocker · 1 major · 0 minor · 0 nit\n\
         \n\
         ```text\n\
         major clippy::unwrap_used (000000000000feed)\n\
         ```"
    );
    Ok(())
}

#[test]
fn gate_outcomes_map_to_check_conclusions() -> Result<(), Box<dyn Error>> {
    let outcomes = [
        (GateConclusion::Pass, CheckConclusion::Success),
        (GateConclusion::Fail, CheckConclusion::Failure),
        (GateConclusion::Neutral, CheckConclusion::Neutral),
    ];
    let mut gates = Vec::new();
    for (index, (conclusion, _)) in outcomes.iter().enumerate() {
        gates.push(gate(
            &format!("ran-{index}"),
            GateOutcome::Ran {
                conclusion: *conclusion,
                steps: StepAttribution::Unknown,
            },
        )?);
    }
    gates.push(gate(
        "errored",
        GateOutcome::Error {
            detail: "timeout".to_owned(),
        },
    )?);
    gates.push(gate("skipped", GateOutcome::Skipped)?);
    let world = decided_world()?;
    let mut inputs = Inputs::new()?;
    inputs.gates = gates;

    written(inputs.publish(&world)?)?;

    let conclusion = |name: &str| -> Result<CheckState, Box<dyn Error>> {
        let check = world.app_check(&format!("assay / gate: {name}"))?;
        Ok(check.output.ok_or("written")?.state)
    };
    for (index, (_, expected)) in outcomes.iter().enumerate() {
        assert_eq!(
            conclusion(&format!("ran-{index}"))?,
            CheckState::Completed(*expected)
        );
    }
    assert_eq!(
        conclusion("errored")?,
        CheckState::Completed(CheckConclusion::Failure)
    );
    assert_eq!(
        conclusion("skipped")?,
        CheckState::Completed(CheckConclusion::Failure)
    );
    Ok(())
}

#[test]
fn an_existing_gate_check_is_updated_not_duplicated() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    let mut inputs = Inputs::new()?;
    inputs.decision_key = format!("sha256:{}", "e".repeat(64));
    world.restamp(&inputs.decision_key);
    written(inputs.publish(&world)?)?;
    world.restamp(KEY);

    written(Inputs::new()?.publish(&world)?)?;

    let gate_checks = world
        .checks
        .borrow()
        .iter()
        .filter(|check| check.name == GATE)
        .count();
    assert_eq!(gate_checks, 1);
    assert_eq!(world.app_check(GATE)?.external_id, KEY);
    Ok(())
}

#[test]
fn a_completed_gate_check_of_this_decision_is_left_alone() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    let inputs = Inputs::new()?;
    written(inputs.publish(&world)?)?;
    world.calls.borrow_mut().clear();

    written(inputs.publish(&world)?)?;

    assert_eq!(world.writes(), [Call::Acquire, Call::Release]);
    Ok(())
}

/// Publishes with the `nth` `call` failing and returns the step the error names, the lock's
/// fate and how many review comments exist afterwards.
fn failing_at(call: Call, nth: usize) -> Result<(Outcome, usize), Box<dyn Error>> {
    let world = decided_world()?;
    world.fail(call, nth);

    let outcome = Inputs::new()?.publish(&world)?;

    let comments = world.review_comments.borrow().len();
    Ok((outcome, comments))
}

#[test]
fn a_failed_verdict_check_read_posts_nothing() -> Result<(), Box<dyn Error>> {
    let (outcome, comments) = failing_at(Call::FindCheck, 1)?;

    assert!(
        matches!(
            outcome,
            Err(FindingsError::Locked {
                step: CommentStep::ReadCheck,
                source: FakeError,
                unlock: Unlock::Released
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(comments, 0);
    Ok(())
}

#[test]
fn a_failed_comment_listing_posts_nothing() -> Result<(), Box<dyn Error>> {
    let (outcome, comments) = failing_at(Call::ReadReviewComments, 1)?;

    assert!(
        matches!(
            outcome,
            Err(FindingsError::Locked {
                step: CommentStep::ReadComments,
                source: FakeError,
                unlock: Unlock::Released
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(comments, 0);
    Ok(())
}

#[test]
fn a_failed_gate_check_read_posts_nothing() -> Result<(), Box<dyn Error>> {
    // The first read is the verdict check's, the second the gate check's.
    let (outcome, comments) = failing_at(Call::FindCheck, 2)?;

    assert!(
        matches!(
            outcome,
            Err(FindingsError::Locked {
                step: CommentStep::ReadGateCheck,
                source: FakeError,
                unlock: Unlock::Released
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(comments, 0);
    Ok(())
}

#[test]
fn a_failed_gate_check_write_names_its_step() -> Result<(), Box<dyn Error>> {
    let (outcome, comments) = failing_at(Call::CreateCheck, 1)?;

    assert!(
        matches!(
            outcome,
            Err(FindingsError::Locked {
                step: CommentStep::WriteGateCheck,
                source: FakeError,
                unlock: Unlock::Released
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(comments, 0, "gate checks are written before comments");
    Ok(())
}

#[test]
fn an_in_progress_gate_check_of_this_decision_is_completed() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;
    world.checks.borrow_mut().push(FakeCheck {
        id: CheckRunId(NonZeroU64::new(77).ok_or("not zero")?),
        head: HEAD.to_owned(),
        name: GATE.to_owned(),
        author: Author::App,
        external_id: KEY.to_owned(),
        status: CheckStatus::InProgress,
        output: None,
        annotations: 0,
    });

    written(Inputs::new()?.publish(&world)?)?;

    let check = world.app_check(GATE)?;
    assert_eq!(
        (check.id.0.get(), check.status),
        (77, CheckStatus::Completed)
    );
    assert!(world.writes().contains(&Call::UpdateCheck));
    assert!(!world.writes().contains(&Call::CreateCheck));
    Ok(())
}

// #332: the reads and writes made under the lock carry its lease's deadline (#290).
#[test]
fn every_call_under_the_lock_carries_the_lease_s_deadline() -> Result<(), Box<dyn Error>> {
    let world = decided_world()?;

    written(Inputs::new()?.publish(&world)?)?;

    let by = Some(lease_deadline(1)?);
    let (held, not_held) = world.bounds_held_and_not();
    assert_eq!(
        held,
        [
            (Call::FindCheck, by),
            (Call::ReadReviewComments, by),
            (Call::FindCheck, by),
            (Call::CreateCheck, by),
            (Call::CreateReviewComment, by),
            (Call::CreateReviewComment, by),
        ]
    );
    assert_eq!(not_held, [], "nothing is called outside the lock");
    Ok(())
}
