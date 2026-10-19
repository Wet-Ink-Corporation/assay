//! `/assay waive` and `/assay override` from command records to the decision (R-08, ADR-22,
//! R3-12, #59): who may issue them, what an override neutralises and when it is void, what a
//! waiver that cannot apply shows, and the events both leave.

use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    ArtifactJob, CommandRejected, CommentAction, CommentEvent, GitHubState, Recorded, waiver_events,
};
use assay_domain::{
    Conclusion, Decision, EventPayload, Fingerprint, GateConclusion, PrClass, ReasonCode, Waiver,
    decide,
};
use serde_json::json;

use crate::decide_fakes::{Scenario, gate_document, gate_finding};
use crate::eligibility_fakes::{APPROVER, By, CONTRIBUTOR, DAILY, World, at, comment, created, fp};
use crate::publish_support::{HEAD_COMMIT, NAME, Run, stamped, written};

/// The commit pushed after the decided head.
const NEXT: char = 'c';

/// A contributor's PR at the scenario's head, nobody's command recorded yet.
fn contributor_pr() -> World {
    World::opened_at(CONTRIBUTOR, HEAD_COMMIT)
}

/// Records `body` as the approver's comment `id` on `world`'s head.
fn record(world: &mut World, id: u64, body: &str) -> Result<(), Box<dyn Error>> {
    let posted = comment(id, APPROVER, 5, body);
    let Recorded::Record(record) = world.state()?.record_command(&created(posted.clone()))? else {
        return Err("a privileged command is recorded".into());
    };
    world.commands.push(record);
    world.comments.insert(posted.id, posted);
    Ok(())
}

/// The scenario's run with its lint temper gate failing on one clippy finding.
fn clippy_failure() -> Result<Run, Box<dyn Error>> {
    let mut scenario = Scenario::new()?;
    let findings = json!([gate_finding(
        "clippy::unwrap_used",
        "src/lib.rs",
        1,
        "pub fn two() {}"
    )]);
    scenario.publish_gate(
        (ArtifactJob::GatesLint, "temper"),
        GateConclusion::Fail,
        gate_document("temper", "fail", &findings),
    )?;
    Run::of(&scenario)
}

/// The fingerprint of the lint temper gate's one finding.
fn clippy_fp(run: &Run) -> Result<Fingerprint, Box<dyn Error>> {
    Ok(run
        .inputs
        .gates
        .iter()
        .flat_map(|gate| &gate.findings)
        .next()
        .ok_or("the lint gate has a finding")?
        .fp)
}

/// `run`'s decision with the override and waivers `state` holds.
fn decided(run: &mut Run, state: &GitHubState) -> Decision {
    run.inputs.override_record = state.override_record();
    run.inputs.waivers = state.waivers();
    decide(&run.rubric, &run.inputs)
}

/// The reasons digest of `run`'s decision before any override.
fn reasons_digest(run: &Run) -> String {
    decide(&run.rubric, &run.inputs).reasons_digest()
}

fn actor(id: u64) -> Result<NonZeroU64, Box<dyn Error>> {
    Ok(NonZeroU64::new(id).ok_or("ids are not zero")?)
}

// --- who may issue a command (P10, R3-02) ---

#[test]
fn an_ineligible_actors_waive_has_no_effect_and_is_recorded_as_rejected()
-> Result<(), Box<dyn Error>> {
    let state = contributor_pr().state()?;
    let posted = comment(40, DAILY, 5, "/assay waive 0123456789abcdef");

    let Err(rejected) = state.record_command(&created(posted.clone())) else {
        return Err("the daily account's command is not recorded".into());
    };

    assert_eq!(
        rejected.event(&posted),
        Some(EventPayload::CommandRejected {
            command: "waive".to_owned(),
            actor_id: actor(DAILY)?,
            reason: "ineligible:maintainer".to_owned(),
        })
    );
    assert_eq!(state.waivers(), []);
    Ok(())
}

#[test]
fn editing_a_comment_into_an_override_has_no_effect_and_is_recorded_as_rejected()
-> Result<(), Box<dyn Error>> {
    let state = contributor_pr().state()?;
    let mut posted = comment(41, APPROVER, 5, "/assay override all sha256:00 break glass");
    posted.updated_at = at(6);
    let edited = CommentEvent {
        action: CommentAction::Edited,
        comment: posted.clone(),
    };

    let recorded = state.record_command(&edited);

    assert_eq!(recorded, Err(CommandRejected::NotCreated));
    assert_eq!(
        CommandRejected::NotCreated.event(&posted),
        Some(EventPayload::CommandRejected {
            command: "override".to_owned(),
            actor_id: actor(APPROVER)?,
            reason: "not-created".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn a_rejected_command_is_named_by_its_grammar_word_never_by_its_text() -> Result<(), Box<dyn Error>>
{
    let state = contributor_pr().state()?;
    let cases = [
        ("/assay approve everything", "unknown", "unknown"),
        ("/assay waive nope", "waive", "malformed"),
    ];
    for (body, command, reason) in cases {
        let posted = comment(42, APPROVER, 5, body);
        let Err(rejected) = state.record_command(&created(posted.clone())) else {
            return Err(format!("{body:?} is rejected").into());
        };

        assert_eq!(
            rejected.event(&posted),
            Some(EventPayload::CommandRejected {
                command: command.to_owned(),
                actor_id: actor(APPROVER)?,
                reason: reason.to_owned(),
            }),
            "{body:?}"
        );
    }
    Ok(())
}

#[test]
fn prose_or_an_authorless_comment_records_no_rejection() -> Result<(), Box<dyn Error>> {
    let state = contributor_pr().state()?;
    let prose = comment(43, APPROVER, 5, "LGTM, but see /assay waive");
    let mut authorless = comment(44, APPROVER, 5, "/assay waive 0123456789abcdef");
    authorless.user = None;

    for posted in [prose, authorless] {
        let Err(rejected) = state.record_command(&created(posted.clone())) else {
            return Err("neither is recorded".into());
        };

        assert_eq!(rejected.event(&posted), None, "{rejected:?}");
    }
    Ok(())
}

// --- what an override neutralises, and when it is void (R3-12, R3-13) ---

#[test]
fn override_all_neutralises_a_gate_failure_and_keeps_its_reasons() -> Result<(), Box<dyn Error>> {
    let mut run = clippy_failure()?;
    let mut world = contributor_pr();
    let body = format!("/assay override all {} break glass", reasons_digest(&run));
    record(&mut world, 41, &body)?;
    let state = world.state()?;

    let decision = decided(&mut run, &state);

    let codes: Vec<ReasonCode> = decision.reasons.iter().map(|r| r.code).collect();
    assert_eq!(decision.conclusion, Conclusion::Neutral);
    assert_eq!(codes, [ReasonCode::GateFailed, ReasonCode::Override]);
    assert_eq!(
        state.override_applied(&decision),
        Some(EventPayload::OverrideApplied {
            who: "approver".to_owned(),
            reason: "break glass".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn override_llm_does_not_neutralise_a_gate_failure() -> Result<(), Box<dyn Error>> {
    let mut run = clippy_failure()?;
    let mut world = contributor_pr();
    let body = format!("/assay override llm {} llm is down", reasons_digest(&run));
    record(&mut world, 41, &body)?;
    let state = world.state()?;

    let decision = decided(&mut run, &state);

    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(state.override_applied(&decision), None);
    Ok(())
}

#[test]
fn an_override_is_void_once_a_reason_is_added() -> Result<(), Box<dyn Error>> {
    let mut run = clippy_failure()?;
    let mut world = contributor_pr();
    let body = format!("/assay override all {} break glass", reasons_digest(&run));
    record(&mut world, 41, &body)?;
    let state = world.state()?;
    run.inputs.pr.classes = vec![PrClass::TrustPath];

    let decision = decided(&mut run, &state);

    let codes: Vec<ReasonCode> = decision.reasons.iter().map(|r| r.code).collect();
    assert_eq!(decision.conclusion, Conclusion::Failure);
    assert_eq!(
        codes,
        [ReasonCode::GateFailed, ReasonCode::TrustPathUnapproved]
    );
    assert_eq!(state.override_applied(&decision), None);
    Ok(())
}

#[test]
fn an_override_lives_for_one_head() -> Result<(), Box<dyn Error>> {
    let run = clippy_failure()?;
    let mut world = contributor_pr();
    let body = format!("/assay override all {} break glass", reasons_digest(&run));
    record(&mut world, 41, &body)?;
    world.commit(NEXT, &[HEAD_COMMIT], By::user(CONTRIBUTOR));
    world.push((HEAD_COMMIT, NEXT), CONTRIBUTOR, 7);
    world.head(NEXT);

    let state = world.state()?;

    assert_eq!(state.override_record(), None);
    Ok(())
}

// --- waivers in the decision: events and the visible refusal (R-08(a)) ---

#[test]
fn an_applied_waiver_is_recorded_as_an_event() -> Result<(), Box<dyn Error>> {
    let mut run = clippy_failure()?;
    let gate = run.inputs.gates.get_mut(1).ok_or("the lint gate")?;
    let finding = gate.findings.first_mut().ok_or("its finding")?;
    finding.rule = Some("exception".to_owned());
    let waived = finding.fp;
    let mut world = contributor_pr();
    record(&mut world, 40, &format!("/assay waive {waived}"))?;
    let state = world.state()?;

    let decision = decided(&mut run, &state);

    assert_eq!(
        waiver_events(&decision).collect::<Vec<_>>(),
        [EventPayload::WaiverApplied {
            who: "approver".to_owned(),
            fp: waived,
        }]
    );
    Ok(())
}

#[test]
fn a_waiver_of_a_clippy_finding_is_refused_with_a_visible_reason() -> Result<(), Box<dyn Error>> {
    let mut run = clippy_failure()?;
    let clippy = clippy_fp(&run)?;
    run.inputs.waivers = vec![Waiver {
        fp: clippy,
        by: "approver".to_owned(),
    }];
    let world = stamped()?;

    written(run.publish(&world))?;

    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(summary.contains("Waivers not applied:"), "{summary}");
    assert!(
        summary.contains(&format!(
            "{clippy} by approver: temper rule clippy::unwrap_used is not a human-decision \
             finding"
        )),
        "{summary}"
    );
    assert_eq!(waiver_events(&decide(&run.rubric, &run.inputs)).count(), 0);
    Ok(())
}

#[test]
fn a_waiver_naming_no_gate_finding_is_refused_with_a_visible_reason() -> Result<(), Box<dyn Error>>
{
    let mut run = clippy_failure()?;
    run.inputs.waivers = vec![Waiver {
        fp: fp(),
        by: "approver".to_owned(),
    }];
    let world = stamped()?;

    written(run.publish(&world))?;

    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(
        summary.contains("0123456789abcdef by approver: no gate finding of this head carries it"),
        "{summary}"
    );
    Ok(())
}

#[test]
fn a_summary_with_every_waiver_applied_has_no_refusal_block() -> Result<(), Box<dyn Error>> {
    let run = clippy_failure()?;
    let world = stamped()?;

    written(run.publish(&world))?;

    let summary = world.app_check(NAME)?.output.ok_or("written")?.summary;
    assert!(!summary.contains("Waivers not applied"), "{summary}");
    Ok(())
}
