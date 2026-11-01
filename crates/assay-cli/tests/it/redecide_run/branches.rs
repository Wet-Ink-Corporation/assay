//! The flow's other branches (#61 DO-11 review round 1, F1, F3, F7): another rubric, a push
//! since the evidence, a ledger that refuses the command record, the deferral flag per trigger,
//! payloads of another repository or head, a Bot's label, how forged evidence is forged, and a
//! register decide-only's own refusal left.

use std::error::Error;
use std::fs;
use std::num::NonZeroU64;
use std::path::Path;

use assay_app::{
    Absence, DeferralRecord, EvidencePointer, Forgery, PushDraft, PushKind, RedecideRefusal, Reply,
    Sha256Digest, StoredBundle, append_push, read_commands, read_rubric,
};
use assay_cli::outputs::{DecideOnlyGuardOutputs, DecideOnlyPrs, DecideTrigger};
use assay_cli::{PayloadMismatch, RedecideRunError, RedecideState, forgery_code};
use assay_domain::{ContentHash, Rubric, RunUid};
use assay_forge::github::{DecideOnlyEvent, HeadRepository, SenderKind};

use super::{
    AUTHOR, DECIDE_ONLY_RUN, LATER, OTHER_HEAD, OWNER, Over, PR, REPO, Scene, assert_silent,
    comment, id, label, override_line, recheck, replied,
};
use crate::redecide_fakes::user;

/// rubric v1 under another version.
fn other_rubric() -> Result<Rubric, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rubric/v1.toml");
    let text = fs::read_to_string(path)?;
    let other = text.replacen("version = \"1\"", "version = \"2\"", 1);
    assert_ne!(other, text, "the version line is replaced");
    Ok(read_rubric(other.as_bytes())?)
}

/// A deferral record of the scene's head, its flag unset.
fn defer(scene: &Scene) -> Result<DeferralRecord, Box<dyn Error>> {
    let deferral = DeferralRecord {
        repo: "Wet-Ink-Corporation/assay".to_owned(),
        pr: id(PR)?,
        head: scene.head(),
        count: 1,
        resets_at: None,
        flag: false,
    };
    // The state stores its own copy; the test keeps one to compare.
    scene.edit_state(|state| state.deferral = Some(deferral.clone()))?;
    Ok(deferral)
}

/// The key decide-only's refusal `refusal` of `evidence` is published under, as
/// `publish_redecide_refusal` computes it.
fn refusal_key(
    evidence: &EvidencePointer,
    refusal: RedecideRefusal,
) -> Result<Sha256Digest, Box<dyn Error>> {
    let reason = refusal.reason();
    let (repository, run_id, attempt) = (
        REPO.to_string(),
        evidence.run_id.to_string(),
        evidence.run_attempt.to_string(),
    );
    let key: Sha256Digest = ContentHash::of_files([
        ("refusal", reason.code.code().as_bytes()),
        ("detail", reason.detail.as_bytes()),
        ("repo_id", repository.as_bytes()),
        ("evidence_run_id", run_id.as_bytes()),
        ("evidence_run_attempt", attempt.as_bytes()),
        ("evidence_sha256", evidence.sha256.as_str().as_bytes()),
    ])
    .to_string()
    .parse()?;
    assert!(
        RedecideRefusal::keyed_over(id(REPO)?, evidence, &key),
        "the key is the one assay-app publishes"
    );
    Ok(key)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn evidence_of_another_rubric_version_is_answered_rerun_needed_and_silent()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let rubric = other_rubric()?;
    let over = Over {
        rubric: &rubric,
        refuse: None,
    };

    let ran = scene.redecide_over(&hub, &recheck()?, (DecideTrigger::Comment, over))?;

    assert_eq!(replied(&hub)?, [Reply::RerunNeeded]);
    assert_silent(&ran)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_push_since_the_evidence_is_answered_head_moved_and_silent() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    append_push(
        &scene.files,
        &PushDraft {
            run_uid: RunUid {
                run_id: id(4300)?,
                ..scene.planned.run.run_env().run_uid
            },
            pr: id(PR)?,
            kind: PushKind::Push {
                before: scene.head(),
                forced: false,
            },
            after: OTHER_HEAD.parse()?,
            sender_id: user(AUTHOR)?,
            at: LATER.parse()?,
        },
    )?;
    let hub = scene.hub()?;

    let ran = scene.redecide(&hub, &recheck()?, DecideTrigger::Comment)?;

    assert_eq!(replied(&hub)?, [Reply::HeadMoved]);
    assert_silent(&ran)
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_command_the_ledger_cannot_record_is_answered_and_writes_no_out() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let guard = DecideOnlyGuardOutputs::new(DecideOnlyPrs::one(id(PR)?), DecideTrigger::Comment);
    let over = Over {
        refuse: Some("commands/"),
        ..scene.as_fixture()
    };
    let event = comment(OWNER, &override_line(&scene), LATER)?;

    let (ran, out) = scene.attempt_over(&hub, &event, (&guard, over))?;

    assert!(
        matches!(ran, Err(RedecideRunError::Append { reply: None, .. })),
        "{ran:?}"
    );
    assert_eq!(replied(&hub)?, [Reply::NotRecorded]);
    assert!(!out.exists(), "no --out");
    assert!(read_commands(&scene.files, id(REPO)?, id(PR)?)?.is_empty());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn the_poll_leaves_the_deferral_flag_and_replies_nothing() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let deferral = defer(&scene)?;
    let hub = scene.hub()?;
    let poll = DecideOnlyEvent::Schedule {
        repository_id: id(REPO)?,
    };

    scene.redecide(&hub, &poll, DecideTrigger::Schedule)?;

    assert_eq!(
        scene.state()?.deferral,
        Some(deferral),
        "the flag stays unset"
    );
    assert!(
        hub.replies.borrow().is_empty(),
        "the poll is never answered"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_label_run_flags_the_deferral_of_the_head() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let deferral = defer(&scene)?;
    let hub = scene.hub()?;

    scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;

    assert_eq!(
        scene.state()?.deferral,
        Some(DeferralRecord {
            flag: true,
            ..deferral
        })
    );
    assert!(hub.replies.borrow().is_empty(), "a label is never answered");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_payload_of_another_repository_or_head_is_refused_before_any_call() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let mut other_repository = recheck()?;
    if let DecideOnlyEvent::Comment(payload) = &mut other_repository {
        payload.repository_id = id(REPO.saturating_add(1))?;
    }
    let mut fork_label = label(&scene)?;
    if let DecideOnlyEvent::Label(payload) = &mut fork_label {
        payload.head_repository = HeadRepository::Other(id(77)?);
    }
    let cases = [
        (
            &other_repository,
            DecideTrigger::Comment,
            PayloadMismatch::Repository,
        ),
        (&fork_label, DecideTrigger::Label, PayloadMismatch::Head),
    ];
    for (event, trigger, expected) in cases {
        let guard = DecideOnlyGuardOutputs::new(DecideOnlyPrs::one(id(PR)?), trigger);

        let (ran, out) = scene.attempt(&hub, event, &guard)?;

        assert!(
            matches!(ran, Err(RedecideRunError::Payload(mismatch)) if mismatch == expected),
            "{ran:?}"
        );
        assert!(!out.exists(), "no --out");
    }
    assert!(scene.log.borrow().is_empty(), "{:?}", scene.log.borrow());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_bots_label_decides_nothing_and_calls_no_port() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let hub = scene.hub()?;
    let mut event = label(&scene)?;
    if let DecideOnlyEvent::Label(payload) = &mut event {
        payload.sender.kind = SenderKind::Bot;
    }

    let ran = scene.redecide(&hub, &event, DecideTrigger::Label)?;

    assert_silent(&ran)?;
    assert!(scene.log.borrow().is_empty(), "{:?}", scene.log.borrow());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn forged_evidence_carries_how_it_is_forged_and_absent_evidence_nothing()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut tampered = scene.bundle.clone();
    tampered.push(b'\n');
    let mut hub = scene.hub()?;
    hub.stored = StoredBundle::Found(tampered);

    let forged = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;
    hub.stored = StoredBundle::Expired;
    let expired = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;

    assert_eq!(
        forged.state()?,
        RedecideState::Refused(RedecideRefusal::Forged)
    );
    let forgery = forged.forgery.as_ref().ok_or("the forgery is carried")?;
    assert!(matches!(forgery, Forgery::Mismatch(_)), "{forgery:?}");
    assert_eq!(forgery_code(forgery), "mismatch");
    assert_eq!(
        expired.state()?,
        RedecideState::Refused(RedecideRefusal::Absent(Absence::Expired))
    );
    assert_eq!(expired.forgery, None);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_register_of_decide_only_s_own_refusal_is_decided_over() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let evidence = scene.state()?.evidence.ok_or("the evidence pointer")?;
    // decide-only's refusal names its own run, which no pointer names.
    let refused_by: NonZeroU64 = DECIDE_ONLY_RUN.parse()?;
    scene.register(
        &scene.head(),
        &refusal_key(&evidence, RedecideRefusal::Absent(Absence::Expired))?,
        refused_by,
    )?;
    let hub = scene.hub()?;

    let ran = scene.redecide(&hub, &label(&scene)?, DecideTrigger::Label)?;

    assert!(ran.has_bundle(), "the evidence is decided again");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "runs git; Miri cannot spawn processes")]
fn a_refusal_register_of_other_evidence_is_not_overwritten() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let evidence = scene.state()?.evidence.ok_or("the evidence pointer")?;
    let other = EvidencePointer {
        run_id: id(4300)?,
        ..evidence
    };
    let refused_by: NonZeroU64 = DECIDE_ONLY_RUN.parse()?;
    scene.register(
        &scene.head(),
        &refusal_key(&other, RedecideRefusal::Forged)?,
        refused_by,
    )?;
    let hub = scene.hub()?;

    let ran = scene.redecide(&hub, &recheck()?, DecideTrigger::Comment)?;

    assert_eq!(replied(&hub)?, [Reply::NoEvidence]);
    assert_silent(&ran)?;
    assert!(
        !scene.log.borrow().iter().any(|call| call == "workflow run"),
        "no evidence read"
    );
    Ok(())
}
