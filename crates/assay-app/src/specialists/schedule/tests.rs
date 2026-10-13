//! The schedule's decisions, driven call by call without threads.

use assay_domain::{Aeu, CallFailure, RateLimitStatus};
use serde_json::{Value, json};

use super::{Joined, Schedule, stamp};
use crate::model_runner::{ModelOutcome, ModelResult, RateLimitObserved, Structured};
use crate::prompt_bundle::Role;
use crate::session_init::InitReadError;
use crate::specialists::report::{Halt, RoleFailure, RoleOutcome};

type Error = &'static str;

const FULL: [Role; 3] = [Role::Bugs, Role::Security, Role::Design];

fn result(outcome: ModelOutcome, micros: u64) -> Joined<Error> {
    Joined::Returned(Ok(ModelResult {
        outcome,
        cost: Aeu::from_micros(micros),
        rate_limits: Vec::new(),
        init: Err(InitReadError::Missing),
    }))
}

fn output(micros: u64) -> Joined<Error> {
    result(ModelOutcome::Ok(Structured::new(json!({}))), micros)
}

/// Every report's role and outcome, once nothing is in flight.
fn outcomes(schedule: Schedule<Error>) -> Vec<(Role, RoleOutcome<Error>)> {
    schedule
        .finish()
        .roles
        .into_iter()
        .map(|report| (report.role, report.outcome))
        .collect()
}

#[test]
fn never_starts_a_fifth_call_while_four_are_in_flight() {
    let mut schedule = Schedule::<Error>::new(&Role::ALL);

    let started: Vec<_> = std::iter::from_fn(|| schedule.next()).collect();

    assert_eq!(started.len(), 4);
    assert_eq!(schedule.in_flight(), 4);
    schedule.settle(0, output(0));
    assert_eq!(schedule.next(), Some((4, Role::Combined)));
}

#[test]
fn a_call_whose_cap_exactly_fills_the_run_cap_starts() {
    let mut schedule = Schedule::<Error>::new(&[Role::Bugs, Role::Security]);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    // Bugs spent its whole 0.60: exactly security's 0.50 of the 1.10 run cap is left.
    schedule.settle(0, output(600_000));

    let next = schedule.next();

    assert_eq!(next, Some((1, Role::Security)));
}

#[test]
fn a_call_one_micro_over_the_run_cap_is_skipped() {
    let mut schedule = Schedule::<Error>::new(&[Role::Bugs, Role::Security]);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    schedule.settle(0, output(600_001));

    let next = schedule.next();

    assert_eq!(next, None);
    assert_eq!(
        outcomes(schedule).pop(),
        Some((Role::Security, RoleOutcome::Skipped(Halt::RunCap)))
    );
}

#[test]
fn a_call_that_does_not_fit_waits_for_the_calls_in_flight_to_settle() {
    let mut schedule = Schedule::<Error>::new(&FULL);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    schedule.settle(0, output(700_000));
    assert_eq!(schedule.next(), Some((1, Role::Security)));

    // 0.70 spent and 0.50 reserved: 0.50 more would pass 1.60, but security may settle lower.
    assert_eq!(schedule.next(), None);
    schedule.settle(1, output(300_000));

    assert_eq!(schedule.next(), Some((2, Role::Design)));
}

#[test]
fn a_retry_starts_before_the_first_attempts_still_due() {
    let mut schedule = Schedule::<Error>::new(&[Role::Bugs, Role::Security]);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    schedule.settle(0, result(ModelOutcome::Malformed, 0));

    let order = [schedule.next(), schedule.next()];

    assert_eq!(order, [Some((0, Role::Bugs)), Some((1, Role::Security))]);
}

#[test]
fn a_retry_is_not_held_to_the_run_cap_that_skips_the_roles_left() {
    let mut schedule = Schedule::<Error>::new(&FULL);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    // 1.20 of 1.60 spent on a malformed answer: neither 0.50 role fits any more.
    schedule.settle(0, result(ModelOutcome::Malformed, 1_200_000));

    let retry = schedule.next();
    schedule.settle(0, output(100_000));

    assert_eq!(retry, Some((0, Role::Bugs)));
    assert_eq!(schedule.next(), None);
    let outcomes = outcomes(schedule);
    assert_eq!(
        outcomes.get(1..),
        Some(
            [
                (Role::Security, RoleOutcome::Skipped(Halt::RunCap)),
                (Role::Design, RoleOutcome::Skipped(Halt::RunCap))
            ]
            .as_slice()
        )
    );
    assert!(matches!(
        outcomes.first(),
        Some((Role::Bugs, RoleOutcome::Completed(_)))
    ));
}

#[test]
fn a_retry_the_paused_stage_never_makes_leaves_the_role_malformed() {
    let mut schedule = Schedule::<Error>::new(&[Role::Bugs, Role::Security]);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    assert_eq!(schedule.next(), Some((1, Role::Security)));
    schedule.settle(0, result(ModelOutcome::Malformed, 0));
    schedule.settle(1, result(ModelOutcome::QuotaExhausted, 0));

    assert_eq!(schedule.next(), None);

    assert_eq!(
        outcomes(schedule).first(),
        Some(&(
            Role::Bugs,
            RoleOutcome::Failed(RoleFailure::Call(CallFailure::Malformed))
        ))
    );
}

#[test]
fn a_second_malformed_answer_fails_the_role_after_two_attempts() {
    let mut schedule = Schedule::<Error>::new(&[Role::Bugs]);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    schedule.settle(0, result(ModelOutcome::Malformed, 10));
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    schedule.settle(0, result(ModelOutcome::Malformed, 20));

    let report = schedule.finish().roles.pop();

    let summary = report.map(|report| (report.outcome, report.attempts, report.cost));
    assert_eq!(
        summary,
        Some((
            RoleOutcome::Failed(RoleFailure::Call(CallFailure::Malformed)),
            2,
            Aeu::from_micros(30)
        ))
    );
}

#[test]
fn an_answer_without_output_is_its_call_failure_and_is_not_retried() {
    let endings = [
        (ModelOutcome::BudgetExhausted, CallFailure::Cap),
        (ModelOutcome::TurnsExhausted, CallFailure::Turns),
        (ModelOutcome::Timeout, CallFailure::Timeout),
        (ModelOutcome::QuotaExhausted, CallFailure::Unavailable),
    ];

    for (ending, failure) in endings {
        let mut schedule = Schedule::<Error>::new(&[Role::Meta]);
        assert_eq!(schedule.next(), Some((0, Role::Meta)));
        schedule.settle(0, result(ending, 0));

        assert_eq!(schedule.next(), None);
        assert_eq!(
            outcomes(schedule),
            [(Role::Meta, RoleOutcome::Failed(RoleFailure::Call(failure)))]
        );
    }
}

#[test]
fn a_rate_limit_warning_pauses_the_stage_but_keeps_the_answer() {
    let mut schedule = Schedule::<Error>::new(&[Role::Bugs, Role::Security]);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));
    let warned = Joined::Returned(Ok(ModelResult {
        outcome: ModelOutcome::Ok(Structured::new(json!({}))),
        cost: Aeu::ZERO,
        rate_limits: vec![RateLimitObserved {
            status: RateLimitStatus::AllowedWarning,
            resets_at: None,
        }],
        init: Err(InitReadError::Missing),
    }));
    schedule.settle(0, warned);

    assert_eq!(schedule.next(), None);

    assert_eq!(
        outcomes(schedule),
        [
            (
                Role::Bugs,
                RoleOutcome::Completed(Structured::new(json!({})))
            ),
            (Role::Security, RoleOutcome::Skipped(Halt::RateLimited))
        ]
    );
}

#[test]
fn a_refused_thread_charges_nothing_and_the_next_role_starts() {
    let mut schedule = Schedule::<Error>::new(&[Role::Bugs, Role::Security]);
    assert_eq!(schedule.next(), Some((0, Role::Bugs)));

    schedule.refused(0, std::io::ErrorKind::WouldBlock);

    assert_eq!(schedule.next(), Some((1, Role::Security)));
    schedule.settle(1, output(0));
    let bugs = schedule.finish().roles.into_iter().next();
    let summary = bugs.map(|report| (report.outcome, report.attempts, report.cost));
    assert_eq!(
        summary,
        Some((
            RoleOutcome::Failed(RoleFailure::Spawn(std::io::ErrorKind::WouldBlock)),
            0,
            Aeu::ZERO
        ))
    );
}

#[test]
fn a_runner_error_is_charged_at_the_role_cap() {
    let mut schedule = Schedule::<Error>::new(&[Role::Design]);
    assert_eq!(schedule.next(), Some((0, Role::Design)));

    schedule.settle(0, Joined::Returned(Err("unreachable")));

    let report = schedule.finish().roles.pop();
    let summary = report.map(|report| (report.outcome, report.cost));
    assert_eq!(
        summary,
        Some((
            RoleOutcome::Failed(RoleFailure::Runner("unreachable")),
            Role::Design.cap()
        ))
    );
}

#[test]
fn stamping_overwrites_the_model_s_role_and_leaves_other_output_alone() {
    let claimed = json!({"source": {"kind": "reviewer", "role": "combined"}, "findings": []});
    let unsourced = json!({"findings": []});
    let not_an_object = json!(["combined"]);

    let stamped = [claimed, unsourced.clone(), not_an_object.clone()]
        .map(|output| stamp(Structured::new(output), Role::CombinedMicro).into_value());

    assert_eq!(
        stamped,
        [
            json!({"source": {"kind": "reviewer", "role": "combined-micro"}, "findings": []}),
            unsourced,
            not_an_object
        ]
    );
    assert_eq!(
        stamp(Structured::new(json!({"source": {}})), Role::Meta).into_value(),
        json!({"source": {"role": Value::from("meta")}})
    );
}
