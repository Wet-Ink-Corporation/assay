//! Commands are events, not state (04 §5, §6, R3-02, R3-24, T21): the closed grammar, what
//! decide-only records from the triggering `created` payload, and how later reads void a record
//! whose comment was deleted or edited.

use std::num::NonZeroU64;

use assay_app::{
    AckedSet, Command, CommandRecord, CommandRejected, CommentAction, CommentEvent, CommentId,
    Ineligible, IssueComment, Parsed, Recorded, ReviewState, Sha256Digest, VoidCause,
    parse_command,
};
use assay_domain::{OverrideScope, RequiredRole};

use crate::eligibility_fakes::{
    APPROVER, By, CONTRIBUTOR, DAILY, DIGEST, FIRST, OWNER, World, at, comment, digest, fp, sha,
    user,
};

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn created(comment: IssueComment) -> CommentEvent {
    CommentEvent {
        action: CommentAction::Created,
        comment,
    }
}

#[test]
fn every_command_of_the_grammar_parses() {
    let cases = [
        ("/assay recheck", Command::Recheck),
        (
            "/assay waive 0123456789abcdef",
            Command::Waive { finding: fp() },
        ),
        (
            &format!("/assay override llm {DIGEST} the model is down"),
            Command::Override {
                scope: OverrideScope::Llm,
                reasons_digest: digest(),
                reason: "the model is down".to_owned(),
            },
        ),
        (
            "/assay dismiss 0123456789abcdef not a defect",
            Command::Dismiss {
                finding: fp(),
                reason: "not a defect".to_owned(),
            },
        ),
        (
            "/assay retry security",
            Command::Retry {
                role: RequiredRole::Security,
            },
        ),
        (
            &format!("/assay weigh-in-ack {DIGEST}"),
            Command::WeighInAck {
                set: AckedSet::Digest(digest()),
            },
        ),
        (
            "/assay weigh-in-ack none",
            Command::WeighInAck {
                set: AckedSet::None,
            },
        ),
    ];

    for (body, command) in cases {
        assert_eq!(parse_command(body), Parsed::Command(command), "{body:?}");
    }
}

#[test]
fn only_the_first_line_is_read_and_a_quote_or_fence_is_never_a_command() {
    assert_eq!(
        parse_command("/assay recheck\r\n/assay waive 0123456789abcdef"),
        Parsed::Command(Command::Recheck)
    );
    for body in [
        "> /assay recheck",
        "```\n/assay recheck\n```",
        " /assay recheck",
        "please /assay recheck",
        "/assayrecheck",
        "thanks\n/assay recheck",
    ] {
        assert_eq!(parse_command(body), Parsed::NotACommand, "{body:?}");
    }
}

#[test]
fn an_unknown_word_or_bad_arguments_are_rejected() {
    assert_eq!(parse_command("/assay approve"), Parsed::Unknown);
    assert_eq!(parse_command("/assay"), Parsed::Unknown);
    for body in [
        "/assay recheck now",
        "/assay waive 0123",
        "/assay waive  0123456789abcdef",
        "/assay override some sha256:00 reason",
        "/assay override llm sha256:00 reason",
        &format!("/assay override llm {DIGEST}"),
        &format!("/assay override llm {DIGEST}  padded"),
        "/assay dismiss 0123456789abcdef",
        "/assay retry everyone",
        "/assay weigh-in-ack all",
        &format!("/assay dismiss 0123456789abcdef {}", "x".repeat(201)),
    ] {
        assert_eq!(parse_command(body), Parsed::Malformed, "{body:?}");
    }
}

/// A contributor's PR, opened at minute 0, approved by nobody.
fn contributor_pr() -> World {
    World::opened_by(CONTRIBUTOR)
}

#[test]
fn an_eligible_waive_is_recorded_with_its_head_and_body_hash() -> Outcome {
    let world = contributor_pr();
    let state = world.state()?;
    let body = "/assay waive 0123456789abcdef";

    let recorded = state.record_command(&created(comment(40, APPROVER, 5, body)))?;

    assert_eq!(
        recorded,
        Recorded::Record(CommandRecord {
            comment_id: CommentId(NonZeroU64::new(40).ok_or("forty")?),
            actor: user(APPROVER),
            created_at: at(5),
            head_sha: sha(FIRST),
            body_sha256: Sha256Digest::of(body.as_bytes()),
            command: Command::Waive { finding: fp() },
        })
    );
    Ok(())
}

#[test]
fn recheck_is_never_recorded() -> Outcome {
    let state = contributor_pr().state()?;

    let recorded = state.record_command(&created(comment(40, CONTRIBUTOR, 5, "/assay recheck")))?;

    assert_eq!(recorded, Recorded::Recheck);
    Ok(())
}

#[test]
fn a_comment_performed_via_an_app_is_rejected() -> Outcome {
    let state = contributor_pr().state()?;
    let mut posted = comment(40, APPROVER, 5, "/assay waive 0123456789abcdef");
    posted.via_app = true;

    let recorded = state.record_command(&created(posted));

    assert_eq!(recorded, Err(CommandRejected::ViaApp));
    Ok(())
}

#[test]
fn editing_a_comment_into_a_command_has_no_effect() -> Outcome {
    let state = contributor_pr().state()?;
    let mut edited = comment(40, APPROVER, 5, "/assay waive 0123456789abcdef");
    edited.updated_at = at(9);

    for action in [CommentAction::Edited, CommentAction::Deleted] {
        let recorded = state.record_command(&CommentEvent {
            action,
            comment: edited.clone(),
        });

        assert_eq!(recorded, Err(CommandRejected::NotCreated), "{action:?}");
    }
    Ok(())
}

#[test]
fn a_command_from_an_ineligible_actor_is_rejected() -> Outcome {
    let state = contributor_pr().state()?;
    let body = "/assay waive 0123456789abcdef";

    let daily = state.record_command(&created(comment(40, DAILY, 5, body)));
    let author = state.record_command(&created(comment(41, CONTRIBUTOR, 5, body)));

    assert_eq!(
        daily,
        Err(CommandRejected::Ineligible(Ineligible::Maintainer))
    );
    assert_eq!(
        author,
        Err(CommandRejected::Ineligible(Ineligible::NotPrivileged))
    );
    Ok(())
}

#[test]
fn a_command_not_later_than_the_push_clock_is_rejected() -> Outcome {
    let state = contributor_pr().state()?;

    let recorded = state.record_command(&created(comment(
        40,
        APPROVER,
        0,
        "/assay waive 0123456789abcdef",
    )));

    assert_eq!(recorded, Err(CommandRejected::BeforePushClock));
    Ok(())
}

#[test]
fn unknown_and_malformed_commands_are_rejected_and_prose_is_not_a_command() -> Outcome {
    let state = contributor_pr().state()?;

    for (body, rejected) in [
        ("/assay approve", CommandRejected::Unknown),
        ("/assay waive nope", CommandRejected::Malformed),
        ("LGTM", CommandRejected::NotACommand),
    ] {
        let recorded = state.record_command(&created(comment(40, APPROVER, 5, body)));

        assert_eq!(recorded, Err(rejected), "{body:?}");
    }
    Ok(())
}

/// A contributor's PR with the approver's waive and override recorded and their comments intact.
fn recorded_commands() -> Result<World, Box<dyn std::error::Error>> {
    let mut world = contributor_pr();
    let state = world.state()?;
    let bodies = [
        (40, "/assay waive 0123456789abcdef".to_owned()),
        (41, format!("/assay override all {DIGEST} break glass")),
        (
            42,
            "/assay dismiss 0123456789abcdef not a defect".to_owned(),
        ),
    ];
    for (id, body) in bodies {
        let posted = comment(id, APPROVER, 5, &body);
        let Recorded::Record(record) = state.record_command(&created(posted.clone()))? else {
            return Err("a privileged command is recorded".into());
        };
        world.commands.push(record);
        world.comments.insert(posted.id, posted);
    }
    Ok(world)
}

#[test]
fn intact_records_become_the_decisions_waivers_and_override() -> Outcome {
    let world = recorded_commands()?;

    let state = world.state()?;

    let waivers = state.waivers();
    assert_eq!(
        waivers
            .iter()
            .map(|waiver| (waiver.fp, waiver.by.as_str()))
            .collect::<Vec<_>>(),
        [(fp(), "approver")]
    );
    let override_record = state.override_record().ok_or("an override")?;
    assert_eq!(
        (
            override_record.scope,
            override_record.reasons_digest.as_str()
        ),
        (OverrideScope::All, DIGEST)
    );
    assert_eq!(state.dismissals().count(), 1);
    assert_eq!(state.voided().count(), 0);
    Ok(())
}

#[test]
fn deleting_or_editing_a_recorded_command_voids_it() -> Outcome {
    let mut world = recorded_commands()?;
    let waive = CommentId(NonZeroU64::new(40).ok_or("forty")?);
    let overriding = CommentId(NonZeroU64::new(41).ok_or("forty-one")?);
    let dismiss = CommentId(NonZeroU64::new(42).ok_or("forty-two")?);
    world.comments.remove(&waive);
    if let Some(edited) = world.comments.get_mut(&overriding) {
        edited.updated_at = at(9);
    }
    if let Some(rewritten) = world.comments.get_mut(&dismiss) {
        rewritten.body = "/assay dismiss 0123456789abcdef changed".to_owned();
    }

    let state = world.state()?;

    let voided: Vec<(CommentId, VoidCause)> = state
        .voided()
        .map(|(record, cause)| (record.comment_id, cause))
        .collect();
    assert_eq!(
        voided,
        [
            (waive, VoidCause::Deleted),
            (overriding, VoidCause::Edited),
            (dismiss, VoidCause::Hash)
        ]
    );
    assert_eq!(state.waivers(), []);
    assert_eq!(state.override_record(), None);
    assert_eq!(state.dismissals().count(), 0);
    Ok(())
}

#[test]
fn a_record_whose_comment_changed_author_or_app_is_void() -> Outcome {
    let mut world = recorded_commands()?;
    let waive = CommentId(NonZeroU64::new(40).ok_or("forty")?);
    let overriding = CommentId(NonZeroU64::new(41).ok_or("forty-one")?);
    if let Some(changed) = world.comments.get_mut(&waive) {
        changed.user = Some(user(OWNER));
    }
    if let Some(changed) = world.comments.get_mut(&overriding) {
        changed.via_app = true;
    }

    let state = world.state()?;

    let causes: Vec<VoidCause> = state.voided().map(|(_, cause)| cause).collect();
    assert_eq!(causes, [VoidCause::Author, VoidCause::App]);
    Ok(())
}

#[test]
fn a_head_bound_command_lapses_on_a_new_head_but_a_dismissal_stays() -> Outcome {
    let mut world = recorded_commands()?;
    world.commit('2', &[FIRST], By::user(CONTRIBUTOR));
    world.push((FIRST, '2'), CONTRIBUTOR, 7);
    world.head('2');

    let state = world.state()?;

    assert_eq!(state.waivers(), []);
    assert_eq!(state.override_record(), None);
    assert_eq!(state.dismissals().count(), 1);
    assert_eq!(state.voided().count(), 0, "lapsed, not voided");
    Ok(())
}

#[test]
fn a_revoked_approver_loses_their_commands_at_once() -> Outcome {
    let mut world = recorded_commands()?;
    let tip = assay_app::EligibilityLists::parse(
        Some(
            "owners = [{ id = 1001, login = \"break-glass\" }]\napprovers = []\n\
             maintainers = [{ id = 124330161, login = \"Ryan-Britton\" }]",
        ),
        Some(crate::eligibility_fakes::LISTS_AGENTS),
    )?;
    world.lists = world.lists.narrowed_to(&tip);
    world.review(APPROVER, ReviewState::Approved, (FIRST, 6));

    let state = world.state()?;

    assert_eq!(
        state.eligibility(user(APPROVER)),
        Err(Ineligible::NotPrivileged)
    );
    assert_eq!(state.waivers(), []);
    assert_eq!(state.dismissals().count(), 0);
    assert_eq!(
        state.owner_approval(),
        assay_domain::OwnerApproval::NotApproved
    );
    Ok(())
}

#[test]
fn an_approver_added_at_the_tip_waits_for_a_new_run() -> Outcome {
    let mut world = World::opened_by(CONTRIBUTOR);
    let base = assay_app::EligibilityLists::parse(
        Some(
            "owners = []\napprovers = []\n\
             maintainers = [{ id = 124330161, login = \"Ryan-Britton\" }]",
        ),
        Some(crate::eligibility_fakes::LISTS_AGENTS),
    )?;
    world.lists = base.narrowed_to(&crate::eligibility_fakes::lists());
    world.review(APPROVER, ReviewState::Approved, (FIRST, 6));

    let state = world.state()?;

    assert_eq!(
        state.owner_approval(),
        assay_domain::OwnerApproval::NotApproved
    );
    Ok(())
}

#[test]
fn two_records_of_one_comment_are_refused() -> Outcome {
    let mut world = recorded_commands()?;
    let repeated = world.commands.first().cloned().ok_or("a record")?;
    world.commands.push(repeated);

    let state = world.state();

    assert!(
        matches!(state, Err(assay_app::StateError::RepeatedCommand { .. })),
        "{state:?}"
    );
    Ok(())
}
