//! `publish` (S8, `docs/architecture/02-architecture.md` §5, 04 §6): the verdict check under
//! the per-head lock, the stamp check that makes a stale writer yield (R3-09), idempotency, the
//! summary comment and the refusal path, against a stateful fake of GitHub and the ledger.

use std::assert_matches;
use std::error::Error;

use assay_app::{
    CheckConclusion, CheckName, CheckState, CheckStatus, ForgeStep, JobResult, Publication,
    PublishError, Published, ReviewState, SUMMARY_MARKER, Stale, Unlock, Verdict, publish,
};
use assay_domain::{Aeu, Deferral, InfraPolicy, LlmStage, Override, OverrideScope, decide};

use crate::decide_fakes::{Scenario, refusal};
use crate::eligibility_fakes::{APPROVER, By, CONTRIBUTOR, FIRST, World};
use crate::plan_fakes::{FakeError, HEAD};
use crate::publish_fakes::{Author, Call, FakeComment, FakeWorld};
use crate::publish_support::{HEAD_COMMIT, KEY, NAME, Run, STAMP, ports, stamped, written};

// --- writing nothing ---

#[test]
fn a_superseded_or_yielding_run_makes_no_call_at_all() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    let publication = Publication {
        target: run.target(),
        verdict: Verdict::Silent,
    };

    let outcome = publish(&publication, &ports(&world));

    assert_matches!(outcome, Ok(Published::Nothing));
    assert_eq!(
        *world.calls.borrow(),
        [],
        "R3-06: no check, comment, upload or ledger write"
    );
    Ok(())
}

#[test]
fn rerunning_with_the_same_inputs_makes_no_write() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    written(run.publish(&world))?;
    world.calls.borrow_mut().clear();

    let outcome = run.publish(&world);

    assert_matches!(outcome, Ok(Published::Unchanged));
    assert_eq!(
        *world.calls.borrow(),
        [Call::FindCheck],
        "one read, nothing else"
    );
    Ok(())
}

#[test]
fn a_pending_decision_is_published_again_since_only_a_final_one_is_a_no_op()
-> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    run.inputs.policy.on_infra_failure = InfraPolicy::Block;
    run.inputs.llm = LlmStage::Deferred {
        reason: Deferral::BudgetDeferred,
    };
    let world = stamped()?;
    let first = written(run.publish(&world))?;

    let second = written(run.publish(&world))?;

    assert_eq!(
        (first.state, second.state),
        (CheckState::InProgress, CheckState::InProgress)
    );
    assert_eq!(world.app_check(NAME)?.status, CheckStatus::InProgress);
    Ok(())
}

// --- the check ---

#[test]
fn a_decision_completes_the_check_under_its_key() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    let check = world.app_check(NAME)?;
    let output = check.output.ok_or("the check was written")?;
    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Success)
    );
    assert_matches!(written.persist, None);
    assert_matches!(written.unlock, Unlock::Released);
    assert_eq!(
        (
            check.external_id.as_str(),
            check.status,
            output.title.as_str()
        ),
        (KEY, CheckStatus::Completed, "assay: success")
    );
    assert!(
        output.summary.starts_with("**assay: success**"),
        "{}",
        output.summary
    );
    assert!(
        output.text.contains(&format!("decision_key: {KEY}")),
        "{}",
        output.text
    );
    assert_eq!(*world.lock.borrow(), None, "the lock is released");
    Ok(())
}

#[test]
fn the_only_ledger_writes_are_the_lock_and_the_evidence() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    assert_eq!(
        world.writes(),
        [
            Call::Upload,
            Call::Acquire,
            Call::RecordEvidence,
            // D2 of #61 (wi-a99de2): the publication register, under the lock, before the check.
            Call::RecordPublished,
            Call::UpdateCheck,
            Call::CreateSummary,
            Call::Release,
        ],
        "R3-06: decide releases no semaphore slot and writes no cost record"
    );
    Ok(())
}

#[test]
fn an_unprotected_base_is_published_as_failure_even_under_an_all_override()
-> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    run.inputs.policy.protected_bases = Vec::new();
    let reasons_digest = decide(&run.rubric, &run.inputs).reasons_digest();
    run.inputs.override_record = Some(Override {
        scope: OverrideScope::All,
        reasons_digest,
        by: "owner".to_owned(),
    });
    let world = stamped()?;

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure),
        "R3-03"
    );
    let output = world.app_check(NAME)?.output.ok_or("written")?;
    assert_eq!(output.title, "assay: failure (base-not-protected)");
    Ok(())
}

#[test]
fn the_check_never_carries_an_annotation_after_any_number_of_updates() -> Result<(), Box<dyn Error>>
{
    let run = Run::new()?;
    let world = stamped()?;

    for _ in 0..5 {
        written(run.publish(&world))?;
        world.restamp(STAMP);
    }

    let updates = world
        .calls
        .borrow()
        .iter()
        .filter(|c| **c == Call::UpdateCheck)
        .count();
    assert_eq!(
        (updates, world.app_check(NAME)?.annotations),
        (5, 0),
        "R3-11"
    );
    Ok(())
}

#[test]
fn the_evidence_pointer_survives_a_same_head_restamp() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    written(run.publish(&world))?;
    let recorded = world.evidence.borrow().clone();

    world.restamp("pending:8:4");

    assert_eq!(world.app_check(NAME)?.external_id, "pending:8:4");
    assert_eq!(
        *world.evidence.borrow(),
        recorded,
        "R3-11: the ledger, not the check, holds it"
    );
    assert_eq!(recorded.len(), 1, "one pointer was written");
    Ok(())
}

#[test]
fn the_check_name_is_scoped_to_its_base() {
    assert_eq!(
        CheckName::for_base("main", "main").as_str(),
        "assay / verdict"
    );
    assert_eq!(
        CheckName::for_base("release", "main").as_str(),
        "assay / verdict @release"
    );
}

#[test]
fn a_pr_into_another_base_writes_only_that_base_s_check() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    "release".clone_into(&mut run.request.plan.pull_request.base_ref);
    let unscoped = stamped()?;
    let scoped = FakeWorld::with_check(HEAD, "assay / verdict @release", STAMP)?;

    let missing = run.publish(&unscoped);
    let published = run.publish(&scoped);

    assert_matches!(missing, Err(PublishError::CheckMissing));
    assert_matches!(published, Ok(Published::Written(_)));
    assert_eq!(
        scoped.app_check("assay / verdict @release")?.external_id,
        KEY
    );
    Ok(())
}

// --- the stamp (R3-09) ---

#[test]
fn two_writers_reading_the_same_stamp_never_both_publish() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::new()?;
    let first = Run::of(&scenario)?;
    // Both writers are run 7, so they share the bundle (and its artifact, whose store refuses
    // other bytes); they differ only in the cost the summary shows.
    let mut second = Run::of(&scenario)?;
    second.cost = Aeu::from_micros(1_250_000);
    let world = stamped()?;
    // The second writer reads the stamp after the first did, then takes the lock first.
    *world.interleave.borrow_mut() = Some(Box::new(move |world: &FakeWorld| {
        let outcome = second.publish(world);
        assert_matches!(
            outcome,
            Ok(Published::Written(_)),
            "the second writer publishes"
        );
    }));

    let outcome = first.publish(&world);

    assert_matches!(
        outcome,
        Ok(Published::Yielded {
            stale: Stale::Moved,
            unlock: Unlock::Released
        })
    );
    let updates = world
        .calls
        .borrow()
        .iter()
        .filter(|c| **c == Call::UpdateCheck)
        .count();
    let check = world.app_check(NAME)?;
    let summary = check.output.ok_or("written")?.summary;
    assert_eq!((updates, check.external_id.as_str()), (1, KEY));
    assert!(
        summary.contains("AEU: 1.25"),
        "the second writer's: {summary}"
    );
    assert_eq!(
        world.evidence.borrow().len(),
        1,
        "only the publisher records evidence"
    );
    Ok(())
}

#[test]
fn a_writer_restamped_by_a_newer_run_yields_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    *world.interleave.borrow_mut() =
        Some(Box::new(|world: &FakeWorld| world.restamp("pending:8:4")));

    let outcome = run.publish(&world);

    assert_matches!(
        outcome,
        Ok(Published::Yielded {
            stale: Stale::Moved,
            unlock: Unlock::Released
        })
    );
    assert_eq!(world.writes(), [Call::Upload, Call::Acquire, Call::Release]);
    assert_eq!(world.app_check(NAME)?.external_id, "pending:8:4");
    Ok(())
}

#[test]
fn a_stamp_that_is_not_this_run_s_yields_without_the_lock() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    for stamp in [
        "pending:8:3",
        "pending:70:3",
        "pending:7:",
        "pending:7:+3",
        "pending:7:3x",
        "pending:7",
        "sha256:eeee",
        "",
    ] {
        let world = FakeWorld::with_check(HEAD, NAME, stamp)?;

        let outcome = run.publish(&world);

        assert_matches!(
            outcome,
            Ok(Published::Yielded {
                stale: Stale::Foreign,
                unlock: Unlock::NotTaken
            }),
            "{stamp:?}"
        );
        assert_eq!(world.writes(), [], "{stamp:?}");
    }
    Ok(())
}

#[test]
fn a_multi_digit_sequence_is_this_run_s_stamp() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = FakeWorld::with_check(HEAD, NAME, "pending:7:1234")?;

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Success)
    );
    Ok(())
}

// --- failures ---

#[test]
fn no_check_for_the_head_is_an_error_and_nothing_is_written() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = FakeWorld::default();

    let outcome = run.publish(&world);

    assert_matches!(outcome, Err(PublishError::CheckMissing));
    assert_eq!(world.writes(), []);
    Ok(())
}

#[test]
fn an_unreadable_check_is_an_error_before_anything_is_written() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.fail(Call::FindCheck, 1);

    let outcome = run.publish(&world);

    assert_matches!(outcome, Err(PublishError::Observe(FakeError)));
    assert_eq!(world.writes(), []);
    Ok(())
}

#[test]
fn an_unreachable_lock_is_an_error() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.fail(Call::Acquire, 1);

    let outcome = run.publish(&world);

    assert_matches!(outcome, Err(PublishError::Lock(FakeError)));
    assert_eq!(world.app_check(NAME)?.output, None);
    Ok(())
}

#[test]
fn a_lock_held_by_another_writer_is_an_error() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    *world.lock.borrow_mut() = Some(99);

    let outcome = run.publish(&world);

    assert_matches!(outcome, Err(PublishError::LockBusy));
    assert_eq!(
        (world.app_check(NAME)?.output, *world.lock.borrow()),
        (None, Some(99))
    );
    Ok(())
}

#[test]
fn a_forge_failure_under_the_lock_names_its_step_and_releases_the_lock()
-> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    for (call, nth, step) in [
        (Call::FindCheck, 2, ForgeStep::ReadCheck),
        (Call::UpdateCheck, 1, ForgeStep::UpdateCheck),
        (Call::FindSummary, 1, ForgeStep::FindSummary),
        (Call::CreateSummary, 1, ForgeStep::WriteSummary),
    ] {
        let world = stamped()?;
        world.fail(call, nth);

        let outcome = run.publish(&world);

        assert_matches!(
            outcome,
            Err(PublishError::Locked {
                step: failed,
                source: FakeError,
                unlock: Unlock::Released,
            }) if failed == step,
            "{step:?}"
        );
        assert_eq!(*world.lock.borrow(), None, "{step:?}: the lock is released");
    }
    Ok(())
}

#[test]
fn a_failed_edit_of_the_summary_comment_is_a_write_summary_error() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.comments.borrow_mut().push(FakeComment {
        id: assay_app::CommentId(std::num::NonZeroU64::MIN),
        author: Author::App,
        body: format!("{SUMMARY_MARKER}\n\nold"),
    });
    world.fail(Call::UpdateSummary, 1);

    let outcome = run.publish(&world);

    assert_matches!(
        outcome,
        Err(PublishError::Locked {
            step: ForgeStep::WriteSummary,
            ..
        })
    );
    Ok(())
}

#[test]
fn a_failed_release_after_a_forge_failure_is_reported() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.fail(Call::UpdateCheck, 1);
    world.fail(Call::Release, 1);

    let outcome = run.publish(&world);

    assert_matches!(
        outcome,
        Err(PublishError::Locked {
            step: ForgeStep::UpdateCheck,
            unlock: Unlock::Failed(FakeError),
            ..
        })
    );
    Ok(())
}

#[test]
fn a_failed_release_after_publishing_is_reported_with_the_write() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.fail(Call::Release, 1);

    let written = written(run.publish(&world))?;

    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Success)
    );
    assert_matches!(written.unlock, Unlock::Failed(FakeError));
    Ok(())
}

#[test]
fn a_failed_release_after_yielding_is_reported_with_the_yield() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    *world.interleave.borrow_mut() =
        Some(Box::new(|world: &FakeWorld| world.restamp("pending:8:4")));
    world.fail(Call::Release, 1);

    let outcome = run.publish(&world);

    assert_matches!(
        outcome,
        Ok(Published::Yielded {
            stale: Stale::Moved,
            unlock: Unlock::Failed(FakeError)
        })
    );
    Ok(())
}

#[test]
fn a_decision_key_that_is_not_a_digest_is_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    STAMP.clone_into(&mut run.inputs.decision_key);
    let world = stamped()?;

    let outcome = run.publish(&world);

    assert_matches!(outcome, Err(PublishError::DecisionKey));
    assert_eq!(*world.calls.borrow(), []);
    Ok(())
}

#[test]
fn inputs_over_another_run_s_evidence_are_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    run.inputs.evidence_key = format!("sha256:{}", "0".repeat(64));
    let world = stamped()?;

    let outcome = run.publish(&world);

    assert_matches!(outcome, Err(PublishError::EvidenceKey));
    assert_eq!(*world.calls.borrow(), []);
    Ok(())
}

/// #58 review F5: the GitHub state snapshot must be of the run's head, or the bundle would
/// record another head's approvals and commands beside this run's decision.
#[test]
fn a_github_snapshot_of_another_head_is_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    run.github = World::opened_by(CONTRIBUTOR).state()?;
    let world = stamped()?;

    let outcome = run.publish(&world);

    assert_matches!(outcome, Err(PublishError::SnapshotHead));
    assert_eq!(*world.calls.borrow(), []);
    Ok(())
}

// --- the summary comment ---

#[test]
fn the_summary_comment_is_edited_when_it_differs() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    world.comments.borrow_mut().push(FakeComment {
        id: assay_app::CommentId(std::num::NonZeroU64::MIN),
        author: Author::App,
        body: format!("{SUMMARY_MARKER}\n\nold"),
    });

    written(run.publish(&world))?;

    let comments = world.comments.borrow();
    let [comment] = comments.as_slice() else {
        return Err("one comment".into());
    };
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert_eq!(comment.body, format!("{SUMMARY_MARKER}\n\n{summary}"));
    assert!(
        world.writes().contains(&Call::UpdateSummary),
        "edited, not posted again"
    );
    Ok(())
}

#[test]
fn a_user_comment_quoting_the_marker_is_left_alone() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    let quoted = format!("{SUMMARY_MARKER}\n\nwritten by someone else");
    world.comments.borrow_mut().push(FakeComment {
        id: assay_app::CommentId(std::num::NonZeroU64::MIN),
        author: Author::Other,
        body: quoted.clone(),
    });

    written(run.publish(&world))?;

    let comments = world.comments.borrow();
    let bodies: Vec<(Author, &str)> = comments
        .iter()
        .map(|c| (c.author, c.body.as_str()))
        .collect();
    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    let own = format!("{SUMMARY_MARKER}\n\n{summary}");
    assert_eq!(
        bodies,
        [
            (Author::Other, quoted.as_str()),
            (Author::App, own.as_str())
        ]
    );
    Ok(())
}

#[test]
fn an_unchanged_summary_comment_is_not_rewritten() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;
    written(run.publish(&world))?;
    world.restamp(STAMP);
    world.calls.borrow_mut().clear();

    written(run.publish(&world))?;

    let writes = world.writes();
    assert!(writes.contains(&Call::UpdateCheck), "{writes:?}");
    assert!(
        !writes.contains(&Call::UpdateSummary) && !writes.contains(&Call::CreateSummary),
        "{writes:?}"
    );
    Ok(())
}

/// P10 (#58): an approval ignored for timing is named in the summary, with both times.
#[test]
fn an_approval_before_the_push_is_reported_as_ignored() -> Result<(), Box<dyn Error>> {
    let mut run = Run::new()?;
    let mut pr = World::opened_by(CONTRIBUTOR);
    pr.commit(HEAD_COMMIT, &[FIRST], By::user(CONTRIBUTOR));
    pr.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 4));
    pr.push((FIRST, HEAD_COMMIT), CONTRIBUTOR, 5);
    pr.head(HEAD_COMMIT);
    run.github = pr.state()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("Ignored approvals:"), "{summary}");
    assert!(
        summary.contains(
            "approval at 2026-10-13T09:04:00Z ignored: before push at 2026-10-13T09:05:00Z \
             (reviewer 1002)"
        ),
        "{summary}"
    );
    Ok(())
}

#[test]
fn a_summary_without_ignored_approvals_has_no_such_block() -> Result<(), Box<dyn Error>> {
    let run = Run::new()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(!summary.contains("Ignored approvals"), "{summary}");
    Ok(())
}

// --- a refusal ---

#[test]
fn a_refusal_fails_the_check_and_persists_nothing() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.plan_job.result = JobResult::Failure;
    let refused = refusal(scenario.authenticate()?)?;
    let run = Run::new()?;
    let world = stamped()?;
    let publication = Publication {
        target: run.target(),
        verdict: Verdict::Refused(&refused),
    };

    let outcome = publish(&publication, &ports(&world));

    let Ok(Published::Written(written)) = outcome else {
        return Err(format!("expected a write, got {outcome:?}").into());
    };
    assert_eq!(
        written.state,
        CheckState::Completed(CheckConclusion::Failure)
    );
    let check = world.app_check(NAME)?;
    let output = check.output.ok_or("written")?;
    assert_eq!(output.title, "assay: failure (gate-error)");
    assert!(
        output.summary.contains("gate-error: plan: failure"),
        "{}",
        output.summary
    );
    assert!(
        check.external_id.starts_with("sha256:"),
        "{}",
        check.external_id
    );
    assert_eq!(
        world.writes(),
        [
            Call::Acquire,
            // D2 of #61 (wi-a99de2): a refusal records the publication register alone.
            Call::RecordPublished,
            Call::UpdateCheck,
            Call::CreateSummary,
            Call::Release
        ],
        "no upload and no evidence"
    );
    Ok(())
}

#[test]
fn the_same_refusal_of_the_same_run_is_published_once() -> Result<(), Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    scenario.request.plan_job.result = JobResult::Failure;
    let refused = refusal(scenario.authenticate()?)?;
    let run = Run::new()?;
    let world = stamped()?;
    let publication = Publication {
        target: run.target(),
        verdict: Verdict::Refused(&refused),
    };
    publish(&publication, &ports(&world))?;

    let outcome = publish(&publication, &ports(&world));

    assert_matches!(outcome, Ok(Published::Unchanged));
    Ok(())
}
