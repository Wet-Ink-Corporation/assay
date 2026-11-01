//! decide-only's re-decision snapshot (D3 A, R3-26, T21): the evidence bundle's recorded
//! range and push facts, the lists narrowed by the base tip, and fresh pushes, reviews and
//! command records judged by the one rule. A push clock that moved refuses.

use std::assert_matches;
use std::error::Error;

use assay_app::{Ineligible, IssueComment, Recorded, RedecideStateError, ReviewState};
use assay_domain::OwnerApproval;

use crate::eligibility_fakes::{
    APPROVER, CONTRIBUTOR, DIGEST, LISTS_OWNERS, OWNER, World, at, comment, created, lists, user,
};
use crate::publish_support::HEAD_COMMIT;
use crate::redecide_fakes::{evidence, redecided, tip_lists};

/// An id in no list at the evidence run's base.
const NEWCOMER: u64 = 3003;

/// The base tip's `.assay/owners` without the approver.
const TIP_WITHOUT_APPROVER: &str = r#"
owners = [{ id = 1001, login = "break-glass" }]
approvers = []
maintainers = [{ id = 124330161, login = "Ryan-Britton" }]
"#;

/// The base tip's `.assay/owners` with a new approver beside the old one.
const TIP_WITH_NEWCOMER: &str = r#"
owners = [{ id = 1001, login = "break-glass" }]
approvers = [{ id = 1002, login = "approver" }, { id = 3003, login = "newcomer" }]
maintainers = [{ id = 124330161, login = "Ryan-Britton" }]
"#;

/// A contributor's PR on the scenario's head, pushed when it was opened (minute 0).
fn contributor_pr() -> World {
    World::opened_at(CONTRIBUTOR, HEAD_COMMIT)
}

/// Records `posted` as the snapshot of `world` judges it, and keeps its comment.
fn record(world: &mut World, posted: IssueComment) -> Result<(), Box<dyn Error>> {
    let Recorded::Record(record) = world.state()?.record_command(&created(posted.clone()))? else {
        return Err("a recorded command".into());
    };
    world.commands.push(record);
    world.comments.insert(posted.id, posted);
    Ok(())
}

#[test]
fn an_unmoved_head_is_judged_by_the_recorded_range_and_pushers() -> Result<(), Box<dyn Error>> {
    let world = World::opened_at(APPROVER, HEAD_COMMIT);
    let evidence = evidence(&world, |_| Ok(()))?;

    let state = redecided(&world, &evidence, &lists())?;

    let recorded = evidence.recorded();
    assert_eq!(state.head(), recorded.head());
    assert_eq!(state.pushers(), recorded.pushers());
    assert_eq!(state.push_clock(), recorded.push_clock());
    assert_eq!(state.commits(), recorded.commits());
    assert_eq!(state.eligibility(user(APPROVER)), Err(Ineligible::Author));
    assert_eq!(state.eligibility(user(OWNER)), Ok(()));
    Ok(())
}

#[test]
fn an_approver_removed_at_the_base_tip_is_ineligible_at_once() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    world.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 5));
    let evidence = evidence(&world, |_| Ok(()))?;
    let tip = tip_lists(TIP_WITHOUT_APPROVER)?;

    let state = redecided(&world, &evidence, &tip)?;

    assert_eq!(
        state.eligibility(user(APPROVER)),
        Err(Ineligible::NotPrivileged)
    );
    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn an_approver_added_at_the_base_tip_is_not_eligible() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    let evidence = evidence(&world, |_| Ok(()))?;
    world.review(NEWCOMER, ReviewState::Approved, (HEAD_COMMIT, 5));
    let tip = tip_lists(TIP_WITH_NEWCOMER)?;

    let state = redecided(&world, &evidence, &tip)?;

    assert_eq!(
        state.eligibility(user(NEWCOMER)),
        Err(Ineligible::NotPrivileged)
    );
    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn an_approval_after_the_push_clock_is_read_fresh() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    let evidence = evidence(&world, |_| Ok(()))?;
    world.review(APPROVER, ReviewState::Approved, (HEAD_COMMIT, 5));

    let state = redecided(&world, &evidence, &lists())?;

    assert_eq!(state.owner_approval(), OwnerApproval::Approved);
    Ok(())
}

/// The owner's override of this head, posted at minute 5.
fn override_comment() -> IssueComment {
    comment(
        50,
        OWNER,
        5,
        &format!("/assay override all {DIGEST} the gate runner is down"),
    )
}

#[test]
fn a_standing_override_record_applies() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    record(&mut world, override_comment())?;
    let evidence = evidence(&world, |_| Ok(()))?;

    let state = redecided(&world, &evidence, &lists())?;

    let applied = state.override_record().ok_or("the override applies")?;
    assert_eq!(applied.by, "break-glass");
    Ok(())
}

/// A case: its name and what happens to the recorded comment.
type Voiding = (&'static str, fn(&mut World));

#[test]
fn an_override_whose_comment_was_deleted_or_edited_no_longer_applies() -> Result<(), Box<dyn Error>>
{
    let cases: [Voiding; 2] = [
        ("deleted", |world| {
            world.comments.clear();
        }),
        ("edited", |world| {
            for posted in world.comments.values_mut() {
                posted.updated_at = at(9);
            }
        }),
    ];
    for (case, void) in cases {
        let mut world = contributor_pr();
        record(&mut world, override_comment())?;
        let evidence = evidence(&world, |_| Ok(()))?;
        void(&mut world);

        let state = redecided(&world, &evidence, &lists())?;

        assert_eq!(state.override_record(), None, "{case}");
        assert_eq!(state.voided().count(), 1, "{case}");
    }
    Ok(())
}

#[test]
fn an_edit_that_turns_a_recorded_comment_into_an_override_adds_none() -> Result<(), Box<dyn Error>>
{
    let mut world = contributor_pr();
    record(
        &mut world,
        comment(
            50,
            OWNER,
            5,
            &format!("/assay waive {}", crate::eligibility_fakes::FP),
        ),
    )?;
    let evidence = evidence(&world, |_| Ok(()))?;
    for posted in world.comments.values_mut() {
        posted.body = override_comment().body;
        posted.updated_at = at(9);
    }

    let state = redecided(&world, &evidence, &lists())?;

    assert_eq!(state.override_record(), None);
    assert_eq!(state.waivers(), Vec::new());
    Ok(())
}

#[test]
fn a_push_record_that_moves_the_head_s_clock_is_pushed_since() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    let evidence = evidence(&world, |_| Ok(()))?;
    world.force_push(('c', HEAD_COMMIT), CONTRIBUTOR, 9);

    let outcome = redecided(&world, &evidence, &lists());

    assert_matches!(outcome, Err(RedecideStateError::PushedSince));
    Ok(())
}

#[test]
fn a_push_to_another_head_after_the_clock_is_pushed_since() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    let evidence = evidence(&world, |_| Ok(()))?;
    world.push((HEAD_COMMIT, 'c'), CONTRIBUTOR, 9);

    let outcome = redecided(&world, &evidence, &lists());

    assert_matches!(outcome, Err(RedecideStateError::PushedSince));
    Ok(())
}

#[test]
fn a_push_record_after_evidence_with_no_push_clock_is_pushed_since() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    world.pushes.clear();
    let evidence = evidence(&world, |_| Ok(()))?;
    assert_eq!(evidence.recorded().push_clock(), None);
    world.push((HEAD_COMMIT, 'c'), CONTRIBUTOR, 9);

    let outcome = redecided(&world, &evidence, &lists());

    assert_matches!(outcome, Err(RedecideStateError::PushedSince));
    Ok(())
}

#[test]
fn a_head_with_no_push_clock_and_no_push_record_is_judged() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    world.pushes.clear();
    let evidence = evidence(&world, |_| Ok(()))?;

    let state = redecided(&world, &evidence, &lists())?;

    assert_eq!(state.push_clock(), None);
    Ok(())
}

#[test]
fn the_lists_are_narrowed_not_replaced_by_the_tip() -> Result<(), Box<dyn Error>> {
    let world = contributor_pr();
    let evidence = evidence(&world, |_| Ok(()))?;
    let tip = tip_lists(TIP_WITH_NEWCOMER)?;

    let state = redecided(&world, &evidence, &tip)?;

    assert_eq!(
        state.lists(),
        &evidence.recorded().lists().narrowed_to(&tip)
    );
    assert_eq!(state.lists(), &tip_lists(LISTS_OWNERS)?);
    Ok(())
}
