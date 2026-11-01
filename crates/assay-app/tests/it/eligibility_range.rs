//! `range_identities` (#60 ST-11): clause (b) of 04 §5 as admit reads it, by the same range
//! reading as decide's snapshot, so the two cannot attribute a range differently.

use std::collections::HashSet;

use assay_app::{
    Actor, CommitIdentity, MAX_RANGE_COMMITS, MergeTree, RangeCommit, RangeError, RangeIdentities,
    RangeListing, SnapshotStep,
};

use crate::eligibility_fakes::{
    BASE, By, CONTRIBUTOR, FIRST, FakeError, OWNER, WEB_FLOW, World, sha, tree, user,
};

type Outcome = Result<(), Box<dyn std::error::Error>>;

/// What [`World::range_identities`] gives for a range of `commits` with `clean` left out.
fn identities(commits: &[(char, u64)], clean: &[char]) -> RangeIdentities {
    let commits: Vec<CommitIdentity> = commits
        .iter()
        .map(|&(commit, by)| CommitIdentity {
            commit: sha(commit),
            authors: vec![Actor::User(user(by))],
            committer: Actor::User(user(by)),
        })
        .collect();
    RangeIdentities::of(&commits, clean.iter().map(|&commit| sha(commit)).collect())
}

#[test]
fn the_range_s_authors_are_read_with_a_clean_merge_from_base_left_out() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit(
        'c',
        &[FIRST, 'd'],
        By::new(Actor::User(user(OWNER)), Actor::User(user(WEB_FLOW))),
    );
    world.push((FIRST, 'c'), OWNER, 5);
    world.head('c');
    world.ancestry.insert((sha('d'), sha(BASE)));
    world
        .merges
        .insert((sha(FIRST), sha('d')), MergeTree::Clean(tree('e')));
    world.trees.insert(sha('c'), tree('e'));

    let range = world.range_identities()?;

    assert_eq!(range, identities(&[(FIRST, CONTRIBUTOR)], &['c']));
    Ok(())
}

#[test]
fn a_merge_that_is_not_clean_counts_its_author() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.commit('c', &[FIRST, 'd'], By::user(OWNER));
    world.push((FIRST, 'c'), OWNER, 5);
    world.head('c');
    world.ancestry.insert((sha('d'), sha(BASE)));
    world
        .merges
        .insert((sha(FIRST), sha('d')), MergeTree::Conflicted);

    let range = world.range_identities()?;

    assert_eq!(
        range,
        identities(&[(FIRST, CONTRIBUTOR), ('c', OWNER)], &[])
    );
    Ok(())
}

#[test]
fn a_range_past_the_cap_leaves_nobody_eligible() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.range = RangeListing::TooMany;

    let range = world.range_identities()?;

    assert_eq!(range, RangeIdentities::of(&[], HashSet::new()));
    Ok(())
}

#[test]
fn a_listing_over_the_cap_is_read_as_too_many() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    let one = RangeCommit {
        commit: sha(FIRST),
        parents: vec![sha(BASE)],
    };
    world.range = RangeListing::Listed(vec![one; MAX_RANGE_COMMITS + 1]);

    let range = world.range_identities()?;

    assert_eq!(range, RangeIdentities::of(&[], HashSet::new()));
    Ok(())
}

#[test]
fn identities_that_do_not_answer_the_range_are_refused() {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.identities_answer = Some(Vec::new());

    let refused = world.range_identities();

    assert!(
        matches!(refused, Err(RangeError::IdentityMismatch)),
        "{refused:?}"
    );
}

#[test]
fn a_git_failure_names_its_step() {
    let mut world = World::opened_by(CONTRIBUTOR);
    world.fail = Some(SnapshotStep::RangeList);

    let refused = world.range_identities();

    assert!(
        matches!(
            refused,
            Err(RangeError::Range {
                step: SnapshotStep::RangeList,
                source: FakeError::Injected(SnapshotStep::RangeList),
            })
        ),
        "{refused:?}"
    );
}
