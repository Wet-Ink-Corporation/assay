//! decide-only's decision bundle (`assay.run-decision/1`, #61 DO-6, D5, J10): what
//! `prepare_decision` writes, what it refuses, and the events it records, in order.

use std::assert_matches;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    CommentId, Contract, ContractValidator, DecisionTarget, DecisionTrigger, EligibilityLists,
    EvidenceBundle, GitHubState, IssueComment, PrepareDecisionError, PreparedBundle, Recorded,
    Redecided, RedecisionRequest, Sha256Digest, SnapshotCommand, Timestamp, TriggerCommand,
    read_decision_bundle, redecision_inputs,
};
use assay_domain::{
    DecisionInputs, EventPayload, Fingerprint, PrClass, Rubric, RunUid, Tier, VoidReason,
};
use serde_json::{Value, json};

use crate::eligibility_fakes::{
    APPROVER, CONTRIBUTOR, LISTS_AGENTS, OWNER, World, at, comment, created, lists, sha, user,
};
use crate::publish_support::HEAD_COMMIT;
use crate::redecide_fakes::{evidence, evidence_of, redecided};
use crate::support::rubric_v1;

/// decide-only's run, which re-decides the evidence run's head.
fn decide_only_run(attempt: u64) -> Result<RunUid, Box<dyn Error>> {
    Ok(RunUid {
        repo_id: NonZeroU64::new(840_213_377).ok_or("not zero")?,
        run_id: NonZeroU64::new(99).ok_or("not zero")?,
        run_attempt: NonZeroU64::new(attempt).ok_or("attempts count from 1")?,
    })
}

/// When decide-only decided.
fn decided_at() -> Result<Timestamp, Box<dyn Error>> {
    Ok("2026-11-01T10:00:00Z".parse()?)
}

/// A contributor's PR on the scenario's head, pushed when it was opened (minute 0).
fn contributor_pr() -> World {
    World::opened_at(CONTRIBUTOR, HEAD_COMMIT)
}

/// Records `posted` as the snapshot of `world` judges it, keeps its comment, and returns the
/// record.
fn record(world: &mut World, posted: &IssueComment) -> Result<SnapshotCommand, Box<dyn Error>> {
    let Recorded::Record(record) = world.state()?.record_command(&created(posted.clone()))? else {
        return Err("a recorded command".into());
    };
    world.commands.push(record.clone());
    world.comments.insert(posted.id, posted.clone());
    Ok(record)
}

/// One re-decision: the evidence, the snapshot over it, its inputs and the rubric.
struct Case {
    evidence: EvidenceBundle,
    state: GitHubState,
    inputs: DecisionInputs,
    rubric: Rubric,
}

impl Case {
    /// The re-decision of `world`'s head from `evidence`, with the base tip's lists `tip`.
    fn over(
        world: &World,
        evidence: EvidenceBundle,
        tip: &EligibilityLists,
    ) -> Result<Self, Box<dyn Error>> {
        let state = redecided(world, &evidence, tip)?;
        let inputs = redecision_inputs(&RedecisionRequest {
            evidence: &evidence,
            state: &state,
            requeues: 0,
        })?;
        Ok(Self {
            evidence,
            state,
            inputs,
            rubric: rubric_v1()?,
        })
    }

    /// The re-decision of a contributor's unchanged PR.
    fn plain() -> Result<Self, Box<dyn Error>> {
        let world = contributor_pr();
        let evidence = evidence(&world, |_| Ok(()))?;
        Self::over(&world, evidence, &lists())
    }

    fn redecided<'a>(&'a self, command: Option<TriggerCommand<'a>>) -> Redecided<'a> {
        Redecided {
            evidence: &self.evidence,
            rubric: &self.rubric,
            inputs: &self.inputs,
            github: &self.state,
            command,
        }
    }

    /// The target of decide-only's first attempt on the evidence's head and base.
    fn target(&self, trigger: DecisionTrigger) -> Result<DecisionTarget<'_>, Box<dyn Error>> {
        Ok(DecisionTarget {
            run_uid: decide_only_run(1)?,
            head_sha: self.evidence.head(),
            base_ref: self.evidence.base_ref(),
            trigger,
            at: decided_at()?,
        })
    }

    /// The bundle of a scheduled re-decision.
    fn scheduled(&self) -> Result<Result<PreparedBundle, PrepareDecisionError>, Box<dyn Error>> {
        Ok(assay_app::prepare_decision(
            &self.target(DecisionTrigger::Schedule)?,
            &self.redecided(None),
        ))
    }
}

/// The comment trigger of `posted`.
fn comment_trigger(posted: &IssueComment) -> Result<DecisionTrigger, Box<dyn Error>> {
    Ok(DecisionTrigger::Comment {
        comment_id: posted.id,
        actor_id: posted.user.ok_or("the comment has an author")?,
    })
}

fn parsed(bundle: &PreparedBundle) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(bundle.bytes())?)
}

#[test]
fn the_bundle_names_its_run_its_evidence_and_its_trigger() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let posted = comment(60, OWNER, 5, "/assay recheck");

    let bundle = assay_app::prepare_decision(
        &case.target(comment_trigger(&posted)?)?,
        &case.redecided(None),
    )?;

    let value = parsed(&bundle)?;
    let keys: Vec<&str> = value
        .as_object()
        .ok_or("an object")?
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "decision",
            "events",
            "evidence",
            "github",
            "inputs",
            "rubric_version",
            "run_uid",
            "schema",
            "trigger"
        ]
    );
    let pointer = case.evidence.pointer();
    assert_eq!(value["schema"], json!("assay.run-decision/1"));
    assert_eq!(
        value["run_uid"],
        json!({"repo_id": 840_213_377, "run_id": 99, "run_attempt": 1})
    );
    assert_eq!(
        value["evidence"],
        json!({
            "run_id": pointer.run_id.get(),
            "run_attempt": 1,
            "sha256": pointer.sha256.as_str(),
        })
    );
    assert_eq!(
        value["trigger"],
        json!({"event": "comment", "comment_id": 60, "actor_id": OWNER})
    );
    assert_eq!(value["inputs"], serde_json::to_value(&case.inputs)?);
    let decision = assay_domain::decide(&case.rubric, &case.inputs);
    assert_eq!(value["decision"], serde_json::to_value(&decision)?);
    assert_eq!(value["rubric_version"], json!(case.rubric.version()));
    assert_eq!(
        value["github"]["head_sha"],
        json!(case.state.head().as_str())
    );
    assert_eq!(value["events"], json!([]), "a recheck records no event");
    assert_eq!(bundle.digest(), &Sha256Digest::of(bundle.bytes()));
    Ok(())
}

#[test]
fn every_trigger_s_bundle_passes_its_contract_and_reads_back() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let validator = ContractValidator::new()?;
    let triggers = [
        DecisionTrigger::Comment {
            comment_id: comment(61, OWNER, 5, "/assay recheck").id,
            actor_id: user(OWNER),
        },
        DecisionTrigger::Label {
            actor_id: user(APPROVER),
        },
        DecisionTrigger::Schedule,
    ];
    for trigger in triggers {
        let bundle = assay_app::prepare_decision(&case.target(trigger)?, &case.redecided(None))?;

        let read = validator.read(Contract::RunDecision, bundle.bytes());
        assert!(read.is_ok(), "{trigger:?}: {read:?}");
        let back = read_decision_bundle(bundle.bytes())?;
        assert_eq!(back.run_uid(), decide_only_run(1)?);
        assert_eq!(back.evidence(), case.evidence.pointer());
        assert_eq!(back.trigger(), trigger);
        assert_eq!(back.inputs(), &case.inputs);
        assert_eq!(back.rubric_version(), case.rubric.version());
        assert_eq!(back.head(), case.state.head());
    }
    Ok(())
}

#[test]
fn a_snapshot_of_another_head_than_the_pr_s_is_refused() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let other = World::opened_at(CONTRIBUTOR, 'c').state()?;
    let redecided = Redecided {
        github: &other,
        ..case.redecided(None)
    };

    let outcome = assay_app::prepare_decision(&case.target(DecisionTrigger::Schedule)?, &redecided);

    assert_matches!(outcome, Err(PrepareDecisionError::SnapshotHead));
    Ok(())
}

#[test]
fn evidence_of_another_head_than_the_pr_s_is_refused() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let moved = World::opened_at(CONTRIBUTOR, 'c').state()?;
    let target = DecisionTarget {
        head_sha: moved.head(),
        ..case.target(DecisionTrigger::Schedule)?
    };
    let redecided = Redecided {
        github: &moved,
        ..case.redecided(None)
    };

    let outcome = assay_app::prepare_decision(&target, &redecided);

    assert_matches!(outcome, Err(PrepareDecisionError::EvidenceHead));
    Ok(())
}

/// One refused case: its name, how it changes the inputs, and the refusal it expects.
type Refused = (
    &'static str,
    fn(&mut DecisionInputs),
    fn(&PrepareDecisionError) -> bool,
);

#[test]
fn inputs_that_do_not_fit_the_evidence_or_the_pr_are_refused() -> Result<(), Box<dyn Error>> {
    let cases: [Refused; 3] = [
        (
            "a decision key that is no digest",
            |inputs| inputs.decision_key = "dec-1".to_owned(),
            |error| matches!(error, PrepareDecisionError::DecisionKey),
        ),
        (
            "another evidence key",
            |inputs| inputs.evidence_key = "sha256:other".to_owned(),
            |error| matches!(error, PrepareDecisionError::EvidenceKey),
        ),
        (
            "another base ref",
            |inputs| inputs.pr.base_ref = "release".to_owned(),
            |error| matches!(error, PrepareDecisionError::BaseRef),
        ),
    ];
    for (name, edit, expected) in cases {
        let mut case = Case::plain()?;
        edit(&mut case.inputs);

        let outcome = case.scheduled()?;

        assert!(
            outcome.as_ref().err().is_some_and(expected),
            "{name}: {outcome:?}"
        );
    }
    Ok(())
}

#[test]
fn evidence_decided_under_another_rubric_version_is_refused() -> Result<(), Box<dyn Error>> {
    let mut case = Case::plain()?;
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rubric/v1.toml");
    let text = std::fs::read_to_string(path)?.replacen("version = \"1\"", "version = \"2\"", 1);
    case.rubric = toml::from_str(&text)?;

    let outcome = case.scheduled()?;

    assert_matches!(outcome, Err(PrepareDecisionError::RubricVersion));
    Ok(())
}

#[test]
fn a_re_run_writes_no_bundle() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let target = DecisionTarget {
        run_uid: decide_only_run(2)?,
        ..case.target(DecisionTrigger::Schedule)?
    };

    let outcome = assay_app::prepare_decision(&target, &case.redecided(None));

    assert_matches!(outcome, Err(PrepareDecisionError::NotFirstAttempt));
    Ok(())
}

#[test]
fn a_command_that_is_not_the_trigger_s_is_refused() -> Result<(), Box<dyn Error>> {
    let mut world = contributor_pr();
    let posted = comment(62, OWNER, 5, "/assay waive 0123456789abcdef");
    let recorded = record(&mut world, &posted)?;
    let evidence = evidence(&world, |_| Ok(()))?;
    let case = Case::over(&world, evidence, &lists())?;
    let other = comment(63, APPROVER, 6, "/assay recheck");
    let triggers = [
        DecisionTrigger::Schedule,
        DecisionTrigger::Label {
            actor_id: user(OWNER),
        },
        comment_trigger(&other)?,
    ];
    for trigger in triggers {
        let outcome = assay_app::prepare_decision(
            &case.target(trigger)?,
            &case.redecided(Some(TriggerCommand::Recorded(&recorded))),
        );

        assert!(
            matches!(outcome, Err(PrepareDecisionError::Trigger)),
            "{trigger:?}: {outcome:?}"
        );
    }
    // The same record, posted on another head.
    let mut elsewhere = recorded;
    elsewhere.head_sha = sha('c');
    let outcome = assay_app::prepare_decision(
        &case.target(comment_trigger(&posted)?)?,
        &case.redecided(Some(TriggerCommand::Recorded(&elsewhere))),
    );
    assert!(
        matches!(outcome, Err(PrepareDecisionError::Trigger)),
        "a record of another head: {outcome:?}"
    );
    Ok(())
}

#[test]
fn a_rejection_that_is_not_the_trigger_s_is_refused() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let posted = comment(81, CONTRIBUTOR, 5, "/assay waive 0123456789abcdef");
    let Err(reason) = case.state.record_command(&created(posted.clone())) else {
        return Err("the contributor's waive is rejected".into());
    };
    let triggers = [
        (
            "another comment",
            comment_trigger(&comment(82, CONTRIBUTOR, 6, "/assay recheck"))?,
        ),
        (
            "another author",
            DecisionTrigger::Comment {
                comment_id: posted.id,
                actor_id: user(APPROVER),
            },
        ),
    ];
    for (name, trigger) in triggers {
        let outcome = assay_app::prepare_decision(
            &case.target(trigger)?,
            &case.redecided(Some(TriggerCommand::Rejected {
                reason,
                comment: &posted,
            })),
        );

        assert!(
            matches!(outcome, Err(PrepareDecisionError::Trigger)),
            "{name}: {outcome:?}"
        );
    }
    Ok(())
}

/// A case: its name, how the recorded comment changes (`None`: it is deleted), and the reason
/// its event gives.
type Voiding = (&'static str, Option<fn(&mut IssueComment)>, VoidReason);

#[test]
fn a_voided_record_s_event_gives_edited_deleted_or_hash() -> Result<(), Box<dyn Error>> {
    let cases: [Voiding; 4] = [
        (
            "edited",
            Some(|posted| posted.updated_at = at(9)),
            VoidReason::Edited,
        ),
        ("deleted", None, VoidReason::Deleted),
        (
            "another author",
            Some(|posted| posted.user = Some(user(APPROVER))),
            VoidReason::Hash,
        ),
        (
            "now via an app",
            Some(|posted| posted.via_app = true),
            VoidReason::Hash,
        ),
    ];
    for (name, change, expected) in cases {
        let mut world = contributor_pr();
        let recorded = record(
            &mut world,
            &comment(90, OWNER, 5, "/assay waive 0123456789abcdef"),
        )?;
        let evidence = evidence(&world, |_| Ok(()))?;
        match change {
            None => {
                world.comments.remove(&recorded.comment_id);
            }
            Some(change) => {
                let stored = world
                    .comments
                    .get_mut(&recorded.comment_id)
                    .ok_or("the comment is kept")?;
                change(stored);
            }
        }
        let case = Case::over(&world, evidence, &lists())?;

        let bundle = case.scheduled()??;

        let back = read_decision_bundle(bundle.bytes())?;
        let payloads: Vec<&EventPayload> =
            back.events().iter().map(|event| &event.payload).collect();
        assert_eq!(
            payloads,
            [&EventPayload::CommandVoided {
                comment_id: recorded.comment_id.0,
                reason: expected,
            }],
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn inputs_of_other_classes_or_another_tier_than_the_evidence_s_are_refused()
-> Result<(), Box<dyn Error>> {
    let cases: [Refused; 2] = [
        (
            "other classes",
            |inputs| inputs.pr.classes = vec![PrClass::TrustPath],
            |error| matches!(error, PrepareDecisionError::Classes),
        ),
        (
            "another tier",
            |inputs| inputs.pr.tier = Tier::Full,
            |error| matches!(error, PrepareDecisionError::Tier),
        ),
    ];
    for (name, edit, expected) in cases {
        let mut case = Case::plain()?;
        edit(&mut case.inputs);

        let outcome = case.scheduled()?;

        assert!(
            outcome.as_ref().err().is_some_and(expected),
            "{name}: {outcome:?}"
        );
    }
    Ok(())
}

/// The kinds of `bundle`'s events, in order.
fn kinds(bundle: &PreparedBundle) -> Result<Vec<String>, Box<dyn Error>> {
    let value = parsed(bundle)?;
    value["events"]
        .as_array()
        .ok_or("events are an array")?
        .iter()
        .map(|event| {
            event["kind"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "an event kind".into())
        })
        .collect()
}

/// A re-decision that records every kind of event, and what each names.
struct EveryKind {
    case: Case,
    bundle: PreparedBundle,
    /// The triggering override's comment.
    posted: CommentId,
    /// The waive record whose comment was deleted.
    deleted: CommentId,
    /// The finding the approver waived on the head.
    fp: Fingerprint,
}

/// The owner's `/assay override all` of the head, over the approver's standing waiver of the
/// static gate's finding and a waive record whose comment was deleted.
fn every_kind() -> Result<EveryKind, Box<dyn Error>> {
    let scenario = crate::waive_carry::raised_scenario()?;
    let fp = crate::decide_fakes::authenticated(scenario.authenticate()?)?
        .gates()
        .first()
        .and_then(|gate| gate.findings.first())
        .ok_or("the static gate has a finding")?
        .fp;
    let mut world = contributor_pr();
    let deleted = record(
        &mut world,
        &comment(70, APPROVER, 3, "/assay waive fedcba9876543210"),
    )?;
    record(
        &mut world,
        &comment(71, APPROVER, 4, &format!("/assay waive {fp}")),
    )?;
    let evidence = evidence_of(&scenario, &world, |_| Ok(()))?;
    world.comments.remove(&deleted.comment_id);
    let unoverridden = Case::over(&world, evidence.clone(), &lists())?;
    let digest = assay_domain::decide(&unoverridden.rubric, &unoverridden.inputs).reasons_digest();
    let posted = comment(
        72,
        OWNER,
        5,
        &format!("/assay override all {digest} the runner is down"),
    );
    let overridden = record(&mut world, &posted)?;
    let case = Case::over(&world, evidence, &lists())?;

    let bundle = assay_app::prepare_decision(
        &case.target(comment_trigger(&posted)?)?,
        &case.redecided(Some(TriggerCommand::Recorded(&overridden))),
    )?;
    Ok(EveryKind {
        case,
        bundle,
        posted: posted.id,
        deleted: deleted.comment_id,
        fp,
    })
}

#[test]
fn the_events_are_the_command_the_voided_records_the_waivers_then_the_override()
-> Result<(), Box<dyn Error>> {
    let every = every_kind()?;

    let back = read_decision_bundle(every.bundle.bytes())?;

    assert_eq!(
        kinds(&every.bundle)?,
        [
            "CommandRecorded",
            "CommandVoided",
            "WaiverApplied",
            "OverrideApplied"
        ]
    );
    let payloads: Vec<&EventPayload> = back.events().iter().map(|event| &event.payload).collect();
    assert_eq!(
        payloads,
        [
            &EventPayload::CommandRecorded {
                command: "override".to_owned(),
                actor_id: user(OWNER).0,
                comment_id: every.posted.0,
            },
            &EventPayload::CommandVoided {
                comment_id: every.deleted.0,
                reason: VoidReason::Deleted,
            },
            &EventPayload::WaiverApplied {
                who: "approver".to_owned(),
                fp: every.fp,
            },
            &EventPayload::OverrideApplied {
                who: "break-glass".to_owned(),
                reason: "the runner is down".to_owned(),
            },
        ]
    );
    Ok(())
}

#[test]
fn each_event_is_redecide_s_naming_decide_only_s_run_and_the_evidence_key()
-> Result<(), Box<dyn Error>> {
    let every = every_kind()?;
    let validator = ContractValidator::new()?;

    let back = read_decision_bundle(every.bundle.bytes())?;

    assert!(
        validator
            .read(Contract::RunDecision, every.bundle.bytes())
            .is_ok(),
        "the bundle passes its contract"
    );
    for event in parsed(&every.bundle)?["events"]
        .as_array()
        .ok_or("events")?
    {
        let read = validator.read(Contract::Event, &serde_json::to_vec(event)?);
        assert!(read.is_ok(), "each event is assay.event/1: {read:?}");
    }
    let envelope: Vec<(&str, u64, &str, &str)> = back
        .events()
        .iter()
        .map(|event| {
            (
                event.job.as_str(),
                event.idx,
                event.stage.as_str(),
                event.ts.as_str(),
            )
        })
        .collect();
    assert_eq!(
        envelope,
        [
            ("redecide", 0, "S7", "2026-11-01T10:00:00Z"),
            ("redecide", 1, "S7", "2026-11-01T10:00:00Z"),
            ("redecide", 2, "S7", "2026-11-01T10:00:00Z"),
            ("redecide", 3, "S7", "2026-11-01T10:00:00Z"),
        ]
    );
    let run = decide_only_run(1)?;
    assert!(
        back.events()
            .iter()
            .all(|event| event.run_uid == run && event.run_key == every.case.inputs.evidence_key),
        "each event names decide-only's run and the evidence key"
    );
    Ok(())
}

#[test]
fn a_rejected_trigger_command_leaves_its_rejection_first() -> Result<(), Box<dyn Error>> {
    let case = Case::plain()?;
    let posted = comment(80, CONTRIBUTOR, 5, "/assay waive 0123456789abcdef");
    let Err(reason) = case.state.record_command(&created(posted.clone())) else {
        return Err("the author's waive is rejected".into());
    };

    let bundle = assay_app::prepare_decision(
        &case.target(comment_trigger(&posted)?)?,
        &case.redecided(Some(TriggerCommand::Rejected {
            reason,
            comment: &posted,
        })),
    )?;

    let back = read_decision_bundle(bundle.bytes())?;
    let payloads: Vec<&EventPayload> = back.events().iter().map(|event| &event.payload).collect();
    assert_eq!(
        payloads,
        [&EventPayload::CommandRejected {
            command: "waive".to_owned(),
            actor_id: user(CONTRIBUTOR).0,
            reason: "ineligible:not-privileged".to_owned(),
        }]
    );
    Ok(())
}

/// The base tip's `.assay/owners`, its owners and approvers the base's, with 64 maintainers the
/// base does not list.
fn tip_with_new_maintainers() -> String {
    let maintainers: Vec<String> = (0..64)
        .map(|n| format!("{{ id = {}, login = \"m{n}\" }}", 5000 + n))
        .collect();
    format!(
        "owners = [{{ id = 1001, login = \"break-glass\" }}]\n\
         approvers = [{{ id = 1002, login = \"approver\" }}]\n\
         maintainers = [{}]\n",
        maintainers.join(", ")
    )
}

#[test]
fn narrowed_lists_past_one_file_s_cap_are_recorded_and_read_back() -> Result<(), Box<dyn Error>> {
    let world = contributor_pr();
    let evidence = evidence(&world, |_| Ok(()))?;
    let tip = EligibilityLists::parse(Some(&tip_with_new_maintainers()), Some(LISTS_AGENTS))?;
    let case = Case::over(&world, evidence, &tip)?;

    let bundle = case.scheduled()??;

    let value = parsed(&bundle)?;
    let maintainers = value["github"]["lists"]["maintainers"]
        .as_array()
        .ok_or("maintainers are an array")?;
    assert_eq!(maintainers.len(), 65, "the base's one and the tip's 64");
    let read = ContractValidator::new()?.read(Contract::RunDecision, bundle.bytes());
    assert!(read.is_ok(), "{read:?}");
    assert_eq!(
        read_decision_bundle(bundle.bytes())?.head(),
        case.state.head()
    );
    Ok(())
}
