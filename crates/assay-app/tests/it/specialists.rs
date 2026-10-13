//! The specialists' fan-out (`docs/architecture/04-pipeline.md` §3, `06-operations.md` §1-§2,
//! issue #95): the roles a plan requires run concurrently through one `ModelRunner`, at most
//! four at a time, the first alone for a head start, under a run cap of the roles' caps.

use std::error::Error;
use std::time::Duration;

use assay_app::{
    Halt, InvalidPrompt, ModelOutcome, PromptError, Role, RoleFailure, RoleOutcome, Specialists,
    run_specialists,
};
use assay_domain::{
    Aeu, BaselineChange, CallFailure, ChangeSet, FileChange, Lines, PackageDiff, RepoGlobs,
    RequiredRole, Tier, tier,
};
use serde_json::Value;

use crate::specialists_fakes::{
    Answer, Fake, Hold, PACK, SCHEMA, Step, Unreachable, bundle, document_from, ended, ran,
    request, tenth_of,
};

/// A head start that never passes in a test: the first call always ends before it.
const LONG: Duration = Duration::from_secs(3_600);

const FULL: [RequiredRole; 3] = [
    RequiredRole::Bugs,
    RequiredRole::Security,
    RequiredRole::Design,
];

/// Runs `roles` through `fake` with the stand-in bundle and pack.
fn run(
    fake: &Fake,
    roles: &[RequiredRole],
    head_start: Duration,
) -> Result<Specialists<Unreachable>, Box<dyn Error>> {
    let bundle = bundle()?;
    Ok(run_specialists(fake, request(roles, &bundle, head_start))?)
}

/// Each report's role and outcome, in report order.
fn outcomes(specialists: &Specialists<Unreachable>) -> Vec<(Role, &RoleOutcome<Unreachable>)> {
    specialists
        .roles
        .iter()
        .map(|report| (report.role, &report.outcome))
        .collect()
}

/// The structured output a completed role reported.
fn output(outcome: &RoleOutcome<Unreachable>) -> Option<&Value> {
    match outcome {
        RoleOutcome::Completed(structured) => Some(structured.as_value()),
        RoleOutcome::Failed(_) | RoleOutcome::Skipped(_) => None,
    }
}

#[test]
fn a_lite_run_calls_combined_once_with_its_limits() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();

    let specialists = run(&fake, &[RequiredRole::Combined], LONG)?;

    let calls = fake.calls_of(Role::Combined);
    let [call] = calls.as_slice() else {
        return Err(format!("{} calls", calls.len()).into());
    };
    assert_eq!(call.user_prompt.as_str(), "PACK\n\nCOMBINED");
    assert_eq!(
        (call.max_turns, call.budget, call.json_schema.as_str()),
        (Role::Combined.max_turns(), Role::Combined.cap(), SCHEMA)
    );
    assert_eq!(
        (
            call.system_prompt_file.to_str(),
            call.add_dir.to_str(),
            call.timeout
        ),
        (
            Some("/base/plugin/prompts/v1/system.md"),
            Some("/work/pr"),
            Duration::from_secs(600)
        )
    );
    let [report] = specialists.roles.as_slice() else {
        return Err("one role was due".into());
    };
    assert_eq!(
        (report.role, report.attempts, report.cost),
        (Role::Combined, 1, tenth_of(Role::Combined))
    );
    assert!(matches!(report.outcome, RoleOutcome::Completed(_)));
    Ok(())
}

#[test]
fn a_full_run_calls_each_role_with_its_limits_and_reports_in_roster_order()
-> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();
    let shuffled = [
        RequiredRole::Design,
        RequiredRole::Bugs,
        RequiredRole::Security,
    ];

    let specialists = run(&fake, &shuffled, Duration::ZERO)?;

    let roles: Vec<Role> = specialists.roles.iter().map(|report| report.role).collect();
    assert_eq!(roles, [Role::Bugs, Role::Security, Role::Design]);
    for role in roles {
        let limits: Vec<_> = fake
            .calls_of(role)
            .iter()
            .map(|call| (call.max_turns, call.budget))
            .collect();
        assert_eq!(limits, [(role.max_turns(), role.cap())], "{}", role.code());
    }
    assert_eq!(specialists.run_cap, Aeu::from_micros(1_600_000));
    Ok(())
}

#[test]
fn a_trust_path_pull_request_adds_meta() -> Result<(), Box<dyn Error>> {
    let changes = ChangeSet {
        files: vec![FileChange::new(
            ".github/workflows/ci.yml",
            Lines::Counted {
                added: 1,
                removed: 1,
            },
        )],
        manifests: Vec::new(),
        include_targets: Vec::new(),
        exec_roots: Vec::new(),
        packages: PackageDiff::Computed(Vec::new()),
        temper_baseline: BaselineChange::Other,
    };
    let plan = tier(&RepoGlobs::default(), &changes);
    assert_eq!(plan.tier, Tier::Full);
    let fake = Fake::answering();

    let specialists = run(&fake, &plan.required_roles, Duration::ZERO)?;

    let roles: Vec<Role> = specialists.roles.iter().map(|report| report.role).collect();
    assert_eq!(
        roles,
        [Role::Bugs, Role::Security, Role::Design, Role::Meta]
    );
    assert_eq!(fake.calls_of(Role::Meta).len(), 1);
    assert_eq!(specialists.run_cap, Aeu::from_micros(1_900_000));
    Ok(())
}

#[test]
fn only_design_and_meta_are_sent_the_intent_after_the_pack() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();
    let bundle = bundle()?;
    let roles = [
        RequiredRole::Bugs,
        RequiredRole::Security,
        RequiredRole::Design,
        RequiredRole::Meta,
    ];
    let mut request = request(&roles, &bundle, Duration::ZERO);
    request.intent = Some("INTENT");

    run_specialists(&fake, request)?;

    let prompts: Vec<(Role, String)> = Role::ALL
        .into_iter()
        .flat_map(|role| {
            fake.calls_of(role)
                .into_iter()
                .map(move |call| (role, call.user_prompt.as_str().to_owned()))
        })
        .collect();
    assert_eq!(
        prompts,
        [
            (Role::Bugs, "PACK\n\nBUGS".to_owned()),
            (Role::Security, "PACK\n\nSECURITY".to_owned()),
            (Role::Design, "PACK\n\nINTENT\n\nDESIGN".to_owned()),
            (Role::Meta, "PACK\n\nINTENT\n\nMETA".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn the_first_call_runs_alone_while_the_head_start_lasts() -> Result<(), Box<dyn Error>> {
    // Bugs waits for a second call to start, but no longer than 100 ms: had the others not
    // waited for the head start, security would have started and released it.
    let fake =
        Fake::new([], Hold::FirstUntilStarted(2)).with_hold_limit(Duration::from_millis(100));

    let specialists = run(&fake, &FULL, LONG)?;

    let steps = fake.seen().steps.clone();
    let at = |wanted: Step| steps.iter().position(|step| *step == wanted);
    let (first_end, second_start) = (at(Step::End(Role::Bugs)), at(Step::Start(Role::Security)));
    assert!(
        first_end.is_some() && first_end < second_start,
        "bugs ended at {first_end:?}, security started at {second_start:?}"
    );
    assert_eq!(steps.first(), Some(&Step::Start(Role::Bugs)));
    assert_eq!(specialists.completed_roles().len(), 3);
    Ok(())
}

#[test]
fn after_the_head_start_the_others_run_while_the_first_still_does() -> Result<(), Box<dyn Error>> {
    let fake = Fake::new([], Hold::FirstUntilStarted(3));

    let specialists = run(&fake, &FULL, Duration::ZERO)?;

    let seen = fake.seen();
    let at = |wanted: Step| seen.steps.iter().position(|step| *step == wanted);
    // With no head start left, the first call's thread need not even reach the runner first.
    let first_end = at(Step::End(Role::Bugs));
    let others_start = [
        at(Step::Start(Role::Security)),
        at(Step::Start(Role::Design)),
    ];
    assert!(
        others_start
            .iter()
            .all(|start| start.is_some() && *start < first_end),
        "the others started at {others_start:?}, the first call ended at {first_end:?}"
    );
    drop(seen);
    assert_eq!(specialists.completed_roles().len(), 3);
    Ok(())
}

#[test]
fn never_more_than_four_model_processes_run_at_once() -> Result<(), Box<dyn Error>> {
    let fake = Fake::new(
        [],
        Hold::UntilInFlight {
            in_flight: 4,
            total: 6,
        },
    );

    let specialists = run(&fake, &RequiredRole::ALL, Duration::ZERO)?;

    let seen = fake.seen();
    assert_eq!((seen.peak, seen.started), (4, 6));
    drop(seen);
    assert_eq!(specialists.completed_roles().len(), 6);
    Ok(())
}

#[test]
fn a_role_listed_twice_runs_once() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();

    let specialists = run(&fake, &[RequiredRole::Bugs, RequiredRole::Bugs], LONG)?;

    assert_eq!((fake.seen().started, specialists.roles.len()), (1, 1));
    Ok(())
}

#[test]
fn no_role_due_makes_no_call_and_waits_for_no_head_start() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();

    // With a head start of an hour, waiting for it would hang the test.
    let specialists = run(&fake, &[], LONG)?;

    assert_eq!(
        (
            specialists.roles.len(),
            specialists.run_cap,
            fake.seen().started
        ),
        (0, Aeu::ZERO, 0)
    );
    Ok(())
}

#[test]
fn a_malformed_answer_is_retried_once_and_both_attempts_are_charged() -> Result<(), Box<dyn Error>>
{
    let fake = Fake::new(
        [(
            Role::Bugs,
            vec![
                Answer::Ran(ended(ModelOutcome::Malformed, Aeu::from_micros(30_000))),
                Answer::Ran(ran(document_from("bugs"), Aeu::from_micros(40_000))),
            ],
        )],
        Hold::Never,
    );

    let specialists = run(&fake, &FULL, LONG)?;

    let bugs = specialists.roles.first().ok_or("bugs was due")?;
    assert_eq!((bugs.attempts, bugs.cost), (2, Aeu::from_micros(70_000)));
    assert!(matches!(bugs.outcome, RoleOutcome::Completed(_)));
    assert_eq!(fake.calls_of(Role::Bugs).len(), 2);
    Ok(())
}

#[test]
fn a_runner_error_is_charged_at_the_cap_and_the_other_roles_still_run() -> Result<(), Box<dyn Error>>
{
    let fake = Fake::new([(Role::Bugs, vec![Answer::Fails])], Hold::Never);

    let specialists = run(&fake, &FULL, LONG)?;

    let bugs = specialists.roles.first().ok_or("bugs was due")?;
    assert_eq!(
        (&bugs.outcome, bugs.cost),
        (
            &RoleOutcome::Failed(RoleFailure::Runner(Unreachable)),
            Role::Bugs.cap()
        )
    );
    assert_eq!(
        specialists.completed_roles(),
        [RequiredRole::Security, RequiredRole::Design].into()
    );
    Ok(())
}

#[test]
fn a_panicking_runner_fails_its_role_and_nothing_more_is_started() -> Result<(), Box<dyn Error>> {
    let fake = Fake::new([(Role::Bugs, vec![Answer::Panics])], Hold::Never);

    let specialists = run(&fake, &FULL, LONG)?;

    assert_eq!(
        outcomes(&specialists),
        [
            (Role::Bugs, &RoleOutcome::Failed(RoleFailure::Panicked)),
            (Role::Security, &RoleOutcome::Skipped(Halt::Panicked)),
            (Role::Design, &RoleOutcome::Skipped(Halt::Panicked)),
        ]
    );
    assert_eq!(specialists.cost(), Role::Bugs.cap());
    assert_eq!(fake.seen().started, 1);
    Ok(())
}

#[test]
fn a_panic_while_others_are_in_flight_lets_them_finish() -> Result<(), Box<dyn Error>> {
    // Security and design wait until all three have started, and bugs until security has
    // ended, so security panics with bugs still in flight.
    let fake = Fake::new(
        [(Role::Security, vec![Answer::Panics])],
        Hold::FirstUntilEnded {
            role: Role::Security,
            started: 3,
        },
    );

    let specialists = run(&fake, &FULL, Duration::ZERO)?;

    let summary: Vec<_> = outcomes(&specialists)
        .into_iter()
        .map(|(role, outcome)| (role, output(outcome).is_some(), outcome))
        .collect();
    assert!(matches!(
        summary.as_slice(),
        [
            (Role::Bugs, true, _),
            (
                Role::Security,
                false,
                RoleOutcome::Failed(RoleFailure::Panicked)
            ),
            (Role::Design, true, _),
        ]
    ));
    assert_eq!(fake.seen().started, 3);
    Ok(())
}

#[test]
fn a_retry_is_given_only_what_is_left_of_the_run_cap() -> Result<(), Box<dyn Error>> {
    let fake = Fake::new(
        [(
            Role::Combined,
            vec![Answer::Ran(ended(
                ModelOutcome::Malformed,
                Aeu::from_micros(150_000),
            ))],
        )],
        Hold::Never,
    );

    let specialists = run(&fake, &[RequiredRole::Combined], LONG)?;

    let budgets: Vec<Aeu> = fake
        .calls_of(Role::Combined)
        .iter()
        .map(|call| call.budget)
        .collect();
    let left = Aeu::from_micros(Role::Combined.cap().micros() - 150_000);
    assert_eq!(budgets, [Role::Combined.cap(), left]);
    assert_eq!(
        specialists.completed_roles(),
        [RequiredRole::Combined].into()
    );
    Ok(())
}

#[test]
fn an_exhausted_quota_skips_every_role_not_yet_started() -> Result<(), Box<dyn Error>> {
    let fake = Fake::new(
        [(
            Role::Bugs,
            vec![Answer::Ran(ended(ModelOutcome::QuotaExhausted, Aeu::ZERO))],
        )],
        Hold::Never,
    );

    let specialists = run(&fake, &FULL, LONG)?;

    assert_eq!(
        outcomes(&specialists),
        [
            (
                Role::Bugs,
                &RoleOutcome::Failed(RoleFailure::Call(CallFailure::Unavailable))
            ),
            (Role::Security, &RoleOutcome::Skipped(Halt::RateLimited)),
            (Role::Design, &RoleOutcome::Skipped(Halt::RateLimited)),
        ]
    );
    Ok(())
}

#[test]
fn a_first_call_past_the_run_cap_leaves_no_room_for_the_rest() -> Result<(), Box<dyn Error>> {
    // The CLI's cap can be passed by up to one response (06 §1): 1.20 of a 1.60 run cap leaves
    // 0.40, less than either remaining role's 0.50.
    let fake = Fake::new(
        [(
            Role::Bugs,
            vec![Answer::Ran(ran(
                document_from("bugs"),
                Aeu::from_micros(1_200_000),
            ))],
        )],
        Hold::Never,
    );

    let specialists = run(&fake, &FULL, LONG)?;

    let skipped: Vec<_> = outcomes(&specialists).into_iter().skip(1).collect();
    assert_eq!(
        skipped,
        [
            (Role::Security, &RoleOutcome::Skipped(Halt::RunCap)),
            (Role::Design, &RoleOutcome::Skipped(Halt::RunCap)),
        ]
    );
    assert_eq!(fake.seen().started, 1);
    Ok(())
}

#[test]
fn the_role_that_ran_is_stamped_over_the_source_role_the_model_wrote() -> Result<(), Box<dyn Error>>
{
    let fake = Fake::new(
        [(
            Role::CombinedMicro,
            vec![Answer::Ran(ran(document_from("combined"), Aeu::ZERO))],
        )],
        Hold::Never,
    );

    let specialists = run(&fake, &[RequiredRole::CombinedMicro], LONG)?;

    let report = specialists.roles.first().ok_or("micro was due")?;
    let role = output(&report.outcome).and_then(|document| document.pointer("/source/role"));
    assert_eq!(role.and_then(Value::as_str), Some("combined-micro"));
    Ok(())
}

#[test]
fn a_context_pack_starting_with_a_dash_is_refused_before_any_call() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();
    let bundle = bundle()?;
    let mut request = request(&FULL, &bundle, LONG);
    request.context_pack = "- a list item";

    let refused = run_specialists(&fake, request);

    let reason = refused.err().as_ref().map(InvalidPrompt::reason);
    assert_eq!(reason, Some(PromptError::LeadingDash));
    assert_eq!(fake.seen().started, 0);
    Ok(())
}

#[test]
fn the_cost_is_every_attempt_of_every_role() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();

    let specialists = run(&fake, &FULL, Duration::ZERO)?;

    let expected = FULL
        .map(|role| tenth_of(Role::from(role)).micros())
        .iter()
        .sum::<u64>();
    assert_eq!(specialists.cost(), Aeu::from_micros(expected));
    Ok(())
}

#[test]
fn completed_roles_names_only_the_roles_that_completed() -> Result<(), Box<dyn Error>> {
    let fake = Fake::new(
        [(
            Role::Security,
            vec![Answer::Ran(ended(
                ModelOutcome::TurnsExhausted,
                Role::Security.cap(),
            ))],
        )],
        Hold::Never,
    );

    let specialists = run(&fake, &FULL, LONG)?;

    assert_eq!(
        specialists.completed_roles(),
        [RequiredRole::Bugs, RequiredRole::Design].into()
    );
    let security = specialists.roles.get(1).map(|report| &report.outcome);
    assert_eq!(
        security,
        Some(&RoleOutcome::Failed(RoleFailure::Call(CallFailure::Turns)))
    );
    Ok(())
}

#[test]
fn every_role_s_prompt_starts_with_the_same_context_pack() -> Result<(), Box<dyn Error>> {
    let fake = Fake::answering();

    run(&fake, &RequiredRole::ALL, Duration::ZERO)?;

    let prefix = format!("{PACK}\n\n");
    let prefixed = fake
        .seen()
        .calls
        .iter()
        .filter(|call| call.user_prompt.as_str().starts_with(&prefix))
        .count();
    assert_eq!(prefixed, 6);
    Ok(())
}
