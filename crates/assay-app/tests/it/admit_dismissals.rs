//! The dismissals of `admit`'s context pack (04 §5, §6, 05 T21, 03 §6d): read from the ledger's
//! command records, kept only from an identity the eligibility rule admits, (a) not the author,
//! (b) no author or committer of the range, (c) no pusher, and only while the comment still
//! stands as recorded.

use std::assert_matches;
use std::collections::{BTreeSet, HashSet};
use std::error::Error;

use assay_app::{
    Actor, AdmitError, CommandRecord, CommentId, CommitId, CommitIdentity, IssueComment, PushDraft,
    PushKind, RangeIdentities, Sha256Digest, Timestamp, UserId, append_command, append_push,
};
use assay_domain::RunUid;
use serde_json::{Value, json};

use crate::admit_fakes::{AGENT, AUTHOR, OWNER, PR, REPO_ID, World, at, nz, plan, this_run};

/// A privileged identity who pushed to the PR.
const PUSHER: u64 = 503;

/// The fingerprint every dismissal names.
const FP: &str = "0123456789abcdef";

/// The dismissal every scenario records.
const REASON: &str = "intended: the caller bounds it";

/// The command line of the owner's dismissal.
fn dismiss() -> String {
    format!("/assay dismiss {FP} {REASON}")
}

/// The owner's dismissal, comment 1, as the pack carries it.
fn owners_dismissal() -> Value {
    json!([{ "comment_id": 1, "actor_id": OWNER, "fp": FP, "reason": REASON }])
}

/// The dismissals of the pack `admit` built.
fn dismissals_of(world: &mut World) -> Result<Value, Box<dyn Error>> {
    let admitted = world.admit(&plan("lite"))?;
    let pack = admitted.pack().ok_or("no pack")?;
    let mut pack: Value = serde_json::from_slice(pack.json())?;
    Ok(pack
        .get_mut("dismissals")
        .map(Value::take)
        .ok_or("no dismissals")?)
}

/// A comment `id` by `actor` saying `body`, recorded by decide-only, and as GitHub shows it now.
fn command(id: u64, actor: u64, body: &str) -> (CommandRecord, IssueComment) {
    let created = at("2026-10-19T10:00:00.000Z");
    let record = CommandRecord {
        run_uid: RunUid {
            repo_id: nz(REPO_ID),
            run_id: nz(100 + id),
            run_attempt: nz(1),
        },
        pr: nz(PR),
        comment_id: CommentId(nz(id)),
        actor_id: UserId(nz(actor)),
        command: body.to_owned(),
        body_sha256: Sha256Digest::of(body.as_bytes()),
        created_at: created,
        at: at("2026-10-19T10:00:05.000Z"),
    };
    let now = IssueComment {
        id: CommentId(nz(id)),
        user: Some(UserId(nz(actor))),
        created_at: Timestamp::from(created),
        updated_at: Timestamp::from(created),
        body: body.to_owned(),
        via_app: false,
    };
    (record, now)
}

/// `record` is in the ledger and its comment reads `now`.
fn recorded(
    world: &mut World,
    (record, now): (CommandRecord, IssueComment),
) -> Result<(), Box<dyn Error>> {
    append_command(&world.ledger, &record)?;
    world
        .comments
        .comments
        .insert(record.comment_id.0.get(), now);
    Ok(())
}

/// `sender` pushed `after` (from `before`) in run `run_id`.
fn pushed(
    world: &World,
    run_id: u64,
    sender: u64,
    (before, after): (CommitId, CommitId),
) -> Result<(), Box<dyn Error>> {
    append_push(
        &world.ledger,
        &PushDraft {
            run_uid: RunUid {
                run_id: nz(run_id),
                ..this_run(1)
            },
            pr: nz(PR),
            kind: PushKind::Push {
                before,
                forced: false,
            },
            after,
            sender_id: UserId(nz(sender)),
            at: at("2026-10-19T09:00:00.000Z"),
        },
    )?;
    Ok(())
}

/// A commit of one repeated hex digit.
fn commit(digit: char) -> Result<CommitId, Box<dyn Error>> {
    Ok(digit.to_string().repeat(40).parse()?)
}

/// A world where the owner, the author, an agent and a pusher are all privileged.
fn everyone_privileged() -> World {
    let mut world = World::new();
    world.identities.privileged =
        BTreeSet::from([OWNER, AUTHOR, AGENT, PUSHER].map(|id| UserId(nz(id))));
    world
}

/// The ledger and comment store hold the dismissal from the owner (comment 1) and from each
/// identity or comment state that voids it: the author (2), an agent (3), an edit (4), a deletion
/// (5), a changed body (6), a pusher (7), a comment created at another time than recorded (9);
/// and the owner's `/assay recheck` (8). The owner's opener record does not make the owner a
/// pusher.
fn dismissals_of_every_kind(world: &mut World) -> Result<(), Box<dyn Error>> {
    let dismiss = dismiss();
    append_push(
        &world.ledger,
        &PushDraft {
            run_uid: RunUid {
                run_id: nz(5),
                ..this_run(1)
            },
            pr: nz(PR),
            kind: PushKind::Opener,
            after: commit('a')?,
            sender_id: UserId(nz(OWNER)),
            at: at("2026-10-19T08:00:00.000Z"),
        },
    )?;
    pushed(world, 9, PUSHER, (commit('a')?, commit('b')?))?;
    let cases = [
        command(1, OWNER, &dismiss),
        command(2, AUTHOR, &dismiss),
        command(3, AGENT, &dismiss),
        command(4, OWNER, &dismiss),
        command(5, OWNER, &dismiss),
        command(6, OWNER, &dismiss),
        command(7, PUSHER, &dismiss),
        command(8, OWNER, "/assay recheck"),
        command(9, OWNER, &dismiss),
    ];
    for (record, mut now) in cases {
        match record.comment_id.0.get() {
            // Edited after it was recorded.
            4 => now.updated_at = Timestamp::from(at("2026-10-19T10:30:00.000Z")),
            // Deleted.
            5 => {
                append_command(&world.ledger, &record)?;
                continue;
            }
            // Its body no longer hashes to the record.
            6 => now.body = format!("{dismiss}."),
            // Never edited, but not the comment created when the record says.
            9 => {
                now.created_at = Timestamp::from(at("2026-10-19T09:59:00.000Z"));
                now.updated_at = now.created_at;
            }
            _ => {}
        }
        recorded(world, (record, now))?;
    }
    Ok(())
}

#[test]
fn only_an_eligible_owners_standing_dismissal_enters_the_pack() -> Result<(), Box<dyn Error>> {
    let mut world = everyone_privileged();
    dismissals_of_every_kind(&mut world)?;

    let dismissals = dismissals_of(&mut world)?;

    assert_eq!(dismissals, owners_dismissal());
    assert_eq!(
        world
            .comments
            .asked
            .borrow()
            .iter()
            .map(|id| id.0.get())
            .collect::<Vec<_>>(),
        [1, 4, 5, 6, 9],
        "only eligible dismiss commands are looked up"
    );
    Ok(())
}

#[test]
fn an_author_or_committer_of_a_range_commit_may_not_dismiss() -> Result<(), Box<dyn Error>> {
    for (author, committer) in [(OWNER, AUTHOR), (AUTHOR, OWNER)] {
        let mut world = everyone_privileged();
        world.range = RangeIdentities::of(
            &[CommitIdentity {
                commit: commit('b')?,
                authors: vec![Actor::User(UserId(nz(author)))],
                committer: Actor::User(UserId(nz(committer))),
            }],
            HashSet::new(),
        );
        recorded(&mut world, command(1, OWNER, &dismiss()))?;

        let dismissals = dismissals_of(&mut world)?;

        assert_eq!(
            dismissals,
            json!([]),
            "author {author}, committer {committer}"
        );
    }
    Ok(())
}

#[test]
fn a_range_commit_attributed_to_no_user_voids_every_dismissal() -> Result<(), Box<dyn Error>> {
    let mut world = everyone_privileged();
    world.range = RangeIdentities::of(
        &[CommitIdentity {
            commit: commit('b')?,
            authors: vec![Actor::NotAUser],
            committer: Actor::User(UserId(nz(AUTHOR))),
        }],
        HashSet::new(),
    );
    recorded(&mut world, command(1, OWNER, &dismiss()))?;

    let dismissals = dismissals_of(&mut world)?;

    assert_eq!(dismissals, json!([]));
    Ok(())
}

#[test]
fn an_empty_range_voids_every_dismissal() -> Result<(), Box<dyn Error>> {
    for clean_merges in [HashSet::new(), HashSet::from([commit('b')?])] {
        let mut world = everyone_privileged();
        let only_merges = [CommitIdentity {
            commit: commit('b')?,
            authors: vec![Actor::User(UserId(nz(AUTHOR)))],
            committer: Actor::User(UserId(nz(AUTHOR))),
        }];
        let commits: &[CommitIdentity] = if clean_merges.is_empty() {
            &[]
        } else {
            &only_merges
        };
        world.range = RangeIdentities::of(commits, clean_merges);
        recorded(&mut world, command(1, OWNER, &dismiss()))?;

        let dismissals = dismissals_of(&mut world)?;

        assert_eq!(dismissals, json!([]), "{} counted commits", commits.len());
    }
    Ok(())
}

#[test]
fn a_clean_merge_from_the_base_disqualifies_neither_its_committer_nor_its_pusher()
-> Result<(), Box<dyn Error>> {
    let merge = commit('c')?;
    let mut world = everyone_privileged();
    world.range = RangeIdentities::of(
        &[
            CommitIdentity {
                commit: commit('b')?,
                authors: vec![Actor::User(UserId(nz(AUTHOR)))],
                committer: Actor::User(UserId(nz(AUTHOR))),
            },
            CommitIdentity {
                commit: commit('c')?,
                authors: vec![Actor::NotAUser],
                committer: Actor::User(UserId(nz(OWNER))),
            },
        ],
        HashSet::from([commit('c')?]),
    );
    pushed(&world, 9, OWNER, (commit('b')?, merge))?;
    recorded(&mut world, command(1, OWNER, &dismiss()))?;

    let dismissals = dismissals_of(&mut world)?;

    assert_eq!(dismissals, owners_dismissal());
    Ok(())
}

#[test]
fn a_comment_store_that_cannot_be_read_fails_admit_before_the_slot() -> Result<(), Box<dyn Error>> {
    let mut world = World::new();
    let (record, _) = command(1, OWNER, &dismiss());
    append_command(&world.ledger, &record)?;
    world.comments.down = true;

    let failed = world.admit(&plan("lite"));

    assert_matches!(failed, Err(AdmitError::Comments(_)));
    assert_eq!(world.refs_held(), Vec::<String>::new());
    Ok(())
}
