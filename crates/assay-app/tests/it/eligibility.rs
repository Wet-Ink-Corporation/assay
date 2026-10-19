//! The one eligibility rule of 04 §5 (P10, T9, R3-01, R3-07, R3-22, R3-23) over a fake PR:
//! who may approve, which approvals count, and what a merge from base or a rebase does.

use assay_app::{
    Actor, CommitRange, IgnoredApproval, Ineligible, MergeTree, PushTrigger, RangeListing,
    ReviewState, push_record,
};
use assay_domain::{Authorship, OwnerApproval, Readiness};

use crate::eligibility_fakes::{
    APPROVER, BASE, By, CONTRIBUTOR, DAILY, FIRST, OWNER, WEB_FLOW, World, at, sha, tree, user,
};

type Outcome = Result<(), Box<dyn std::error::Error>>;

#[test]
fn an_owner_authored_pr_leaves_the_owner_ineligible_and_the_approver_eligible() -> Outcome {
    let mut world = World::opened_by(OWNER);
    world.review(OWNER, ReviewState::Approved, (FIRST, 5));
    world.review(APPROVER, ReviewState::Approved, (FIRST, 6));

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Err(Ineligible::Author));
    assert_eq!(state.eligibility(user(APPROVER)), Ok(()));
    assert_eq!(state.owner_approval(), OwnerApproval::Approved);
    Ok(())
}

#[test]
fn the_owner_approval_alone_on_their_own_pr_does_not_count() -> Outcome {
    let mut world = World::opened_by(OWNER);
    world.review(OWNER, ReviewState::Approved, (FIRST, 5));

    let state = world.state()?;

    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn the_daily_account_is_never_eligible_even_on_someone_elses_pr() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.review(DAILY, ReviewState::Approved, (FIRST, 5));

    let state = world.state()?;

    assert_eq!(state.eligibility(user(DAILY)), Err(Ineligible::Maintainer));
    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn with_no_privileged_identity_no_pr_is_approved_and_nothing_fails() -> Outcome {
    let mut world = World::opened_by(DAILY);
    world.lists = assay_app::EligibilityLists::parse(
        Some(
            "owners = []\napprovers = []\nmaintainers = [{ id = 124330161, login = \"Ryan-Britton\" }]",
        ),
        Some(crate::eligibility_fakes::LISTS_AGENTS),
    )?;
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));

    let state = world.state()?;

    assert_eq!(
        state.eligibility(user(APPROVER)),
        Err(Ineligible::NotPrivileged)
    );
    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    assert_eq!(state.authorship(), Authorship::Agent);
    Ok(())
}

#[test]
fn a_pr_by_an_identity_off_the_agent_list_is_not_agent_authored() -> Outcome {
    let world = World::opened_by(CONTRIBUTOR);

    let state = world.state()?;

    assert_eq!(state.authorship(), Authorship::Other);
    Ok(())
}

#[test]
fn an_approval_on_an_older_head_is_ignored() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));
    world.commit('2', &[FIRST], By::user(CONTRIBUTOR));
    world.push((FIRST, '2'), CONTRIBUTOR, 6);
    world.head('2');

    let state = world.state()?;

    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn approved_then_changes_requested_on_the_same_head_does_not_count() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));
    world.review(APPROVER, ReviewState::ChangesRequested, (FIRST, 6));

    let state = world.state()?;

    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn a_later_comment_review_leaves_an_approval_standing() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));
    world.review(APPROVER, ReviewState::Commented, (FIRST, 6));

    let state = world.state()?;

    assert_eq!(state.owner_approval(), OwnerApproval::Approved);
    Ok(())
}

#[test]
fn a_dismissed_approval_does_not_count() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.review(APPROVER, ReviewState::Dismissed, (FIRST, 5));

    let state = world.state()?;

    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn an_approval_before_the_push_clock_is_ignored_and_reported() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit('2', &[FIRST], By::user(CONTRIBUTOR));
    world.review(APPROVER, ReviewState::Approved, ('2', 4));
    world.push((FIRST, '2'), CONTRIBUTOR, 5);
    world.head('2');

    let state = world.state()?;

    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    let ignored: Vec<IgnoredApproval> = state.ignored_approvals().collect();
    assert_eq!(ignored.len(), 1);
    let first = ignored.first().ok_or("one ignored approval")?;
    assert_eq!(first.reviewer, user(APPROVER));
    assert_eq!(
        first.to_string(),
        "approval at 2026-10-13T09:04:00Z ignored: before push at 2026-10-13T09:05:00Z"
    );
    Ok(())
}

#[test]
fn the_push_clock_is_the_latest_push_onto_the_head() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit('2', &[FIRST], By::user(CONTRIBUTOR));
    world.push((FIRST, '2'), CONTRIBUTOR, 3);
    world.force_push(('2', FIRST), CONTRIBUTOR, 4);
    world.force_push((FIRST, '2'), CONTRIBUTOR, 8);
    world.review(APPROVER, ReviewState::Approved, ('2', 6));
    world.head('2');

    let state = world.state()?;

    assert_eq!(state.push_clock(), Some(at(8)));
    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn a_head_with_no_push_record_has_no_clock_and_no_approval() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.pushes.clear();
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));

    let state = world.state()?;

    assert_eq!(state.push_clock(), None);
    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

/// The merge commit "Update branch" writes.
const MERGE: char = 'c';
/// The base's tip it merged.
const TIP: char = 'd';
/// The commit "Update with rebase" writes.
const REBASED: char = 'a';

/// A contributor's PR the owner brought up to date with "Update branch" (merge): the merge
/// commit [`MERGE`] (author the owner, committer web-flow) on the PR's commit and [`TIP`].
fn updated_branch(merge: MergeTree) -> World {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit(
        MERGE,
        &[FIRST, TIP],
        By::new(Actor::User(user(OWNER)), Actor::User(user(WEB_FLOW))),
    );
    world.push((FIRST, MERGE), OWNER, 5);
    world.head(MERGE);
    world.ancestry.insert((sha(TIP), sha(BASE)));
    world.merges.insert((sha(FIRST), sha(TIP)), merge);
    world.trees.insert(sha(MERGE), tree('e'));
    world.review(OWNER, ReviewState::Approved, (MERGE, 6));
    world
}

#[test]
fn update_branch_by_merge_keeps_the_owner_eligible_as_committer_and_pusher() -> Outcome {
    let world = updated_branch(MergeTree::Clean(tree('e')));

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Ok(()));
    assert_eq!(state.owner_approval(), OwnerApproval::Approved);
    assert!(state.pushers().is_empty(), "{:?}", state.pushers());
    Ok(())
}

#[test]
fn a_merge_whose_tree_is_not_the_clean_merge_disqualifies_its_author() -> Outcome {
    let world = updated_branch(MergeTree::Clean(tree('f')));

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Err(Ineligible::InRange));
    Ok(())
}

#[test]
fn a_conflicted_merge_disqualifies_its_author() -> Outcome {
    let world = updated_branch(MergeTree::Conflicted);

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Err(Ineligible::InRange));
    Ok(())
}

#[test]
fn a_merge_of_a_commit_off_the_base_disqualifies_its_author() -> Outcome {
    let mut world = updated_branch(MergeTree::Clean(tree('e')));
    world.ancestry.clear();

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Err(Ineligible::InRange));
    Ok(())
}

#[test]
fn a_merge_pushed_on_another_commit_disqualifies_its_pusher() -> Outcome {
    let mut world = updated_branch(MergeTree::Clean(tree('e')));
    world.pushes.retain(|record| record.after != sha(MERGE));
    world.force_push(('9', MERGE), OWNER, 5);

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Err(Ineligible::InRange));
    assert!(state.pushers().contains(&user(OWNER)));
    Ok(())
}

#[test]
fn update_with_rebase_disqualifies_the_owner() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.range = RangeListing::Listed(Vec::new());
    world.commit(
        REBASED,
        &[TIP],
        By::new(Actor::User(user(CONTRIBUTOR)), Actor::User(user(OWNER))),
    );
    world.force_push((FIRST, REBASED), OWNER, 5);
    world.head(REBASED);
    world.review(OWNER, ReviewState::Approved, (REBASED, 6));

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Err(Ineligible::InRange));
    assert_eq!(state.owner_approval(), OwnerApproval::NotApproved);
    Ok(())
}

#[test]
fn a_push_by_the_owner_disqualifies_them_even_when_they_wrote_no_commit() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit('2', &[FIRST], By::user(CONTRIBUTOR));
    world.push((FIRST, '2'), OWNER, 5);
    world.head('2');

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Err(Ineligible::Pusher));
    Ok(())
}

#[test]
fn a_commit_no_github_user_made_leaves_every_owner_ineligible_but_not_the_approver() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit(
        '2',
        &[FIRST],
        By::new(Actor::User(user(CONTRIBUTOR)), Actor::NotAUser),
    );
    world.push((FIRST, '2'), CONTRIBUTOR, 5);
    world.head('2');

    let state = world.state()?;

    assert_eq!(
        state.eligibility(user(OWNER)),
        Err(Ineligible::UnattributedRange)
    );
    assert_eq!(state.eligibility(user(APPROVER)), Ok(()));
    Ok(())
}

#[test]
fn an_unlinked_author_also_leaves_every_owner_ineligible() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit(
        '2',
        &[FIRST],
        By::new(Actor::NotAUser, Actor::User(user(CONTRIBUTOR))),
    );
    world.push((FIRST, '2'), CONTRIBUTOR, 5);
    world.head('2');

    let state = world.state()?;

    assert_eq!(
        state.eligibility(user(OWNER)),
        Err(Ineligible::UnattributedRange)
    );
    Ok(())
}

#[test]
fn web_flow_as_committer_is_an_identity_not_a_taint() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit(
        '2',
        &[FIRST],
        By::new(Actor::User(user(CONTRIBUTOR)), Actor::User(user(WEB_FLOW))),
    );
    world.push((FIRST, '2'), CONTRIBUTOR, 5);
    world.head('2');

    let state = world.state()?;

    assert_eq!(state.eligibility(user(OWNER)), Ok(()));
    Ok(())
}

#[test]
fn web_flow_as_author_hides_the_author_and_taints_the_range() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit('2', &[FIRST], By::user(WEB_FLOW));
    world.push((FIRST, '2'), CONTRIBUTOR, 5);
    world.head('2');

    let state = world.state()?;

    assert_eq!(
        state.eligibility(user(OWNER)),
        Err(Ineligible::UnattributedRange)
    );
    Ok(())
}

#[test]
fn co_author_trailers_are_ignored() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit(
        '2',
        &[FIRST],
        By {
            authors: vec![
                Actor::User(user(CONTRIBUTOR)),
                Actor::User(user(APPROVER)),
                Actor::NotAUser,
            ],
            committer: Actor::User(user(CONTRIBUTOR)),
        },
    );
    world.push((FIRST, '2'), CONTRIBUTOR, 5);
    world.head('2');

    let state = world.state()?;

    assert_eq!(state.eligibility(user(APPROVER)), Ok(()));
    assert_eq!(state.eligibility(user(OWNER)), Ok(()));
    Ok(())
}

#[test]
fn a_range_past_250_commits_needs_a_split_and_approves_nothing() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.range = RangeListing::TooMany;
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));

    let state = world.state()?;
    let snapshot = state.snapshot(Readiness::Ready, 0);

    assert_eq!(snapshot.commits, CommitRange::NeedsSplit);
    assert_eq!(snapshot.owner_approval, OwnerApproval::NotApproved);
    assert_eq!(
        state.eligibility(user(APPROVER)),
        Err(Ineligible::RangeTooLarge)
    );
    Ok(())
}

#[test]
fn only_synchronize_is_a_push_and_opened_is_the_opener() {
    let opened = push_record(PushTrigger::Opened {
        head: sha(FIRST),
        sender: user(CONTRIBUTOR),
        at: at(0),
    });
    let synchronize = push_record(PushTrigger::Synchronize {
        before: sha(FIRST),
        after: sha('2'),
        sender: user(OWNER),
        forced: false,
        at: at(1),
    });

    assert_eq!(
        opened.map(|record| (record.kind, record.before, record.sender)),
        Some((assay_app::PushKind::Opener, None, user(CONTRIBUTOR)))
    );
    assert_eq!(
        synchronize.map(|record| (record.kind, record.before, record.at)),
        Some((assay_app::PushKind::Push, Some(sha(FIRST)), at(1)))
    );
}

#[test]
fn a_non_push_event_never_records_a_push() {
    for trigger in [
        PushTrigger::Reopened,
        PushTrigger::ReadyForReview,
        PushTrigger::Edited,
        PushTrigger::BaseEdited,
        PushTrigger::Dispatch,
        PushTrigger::Rerun,
        PushTrigger::Requeue,
    ] {
        let label = format!("{trigger:?}");

        assert_eq!(push_record(trigger), None, "{label} recorded a push");
    }
}

#[test]
fn a_rerun_or_requeue_does_not_move_the_push_clock() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.review(APPROVER, ReviewState::Approved, (FIRST, 5));
    let before = world.state()?;

    for trigger in [PushTrigger::Rerun, PushTrigger::Requeue] {
        world.pushes.extend(push_record(trigger));
    }
    let after = world.state()?;

    assert_eq!(after.push_clock(), before.push_clock());
    assert_eq!(after.owner_approval(), OwnerApproval::Approved);
    Ok(())
}
