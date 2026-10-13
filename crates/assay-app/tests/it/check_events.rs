//! `check_local`'s `assay.event/1` stream (#130, 03 §6): each job's events numbered from 0 in
//! order, every one valid, the recorded stream the mod's tests read, and a sink or clock that
//! fails stopping the check.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use assay_app::{
    CheckError, CheckPorts, Contract, EventSink, LocalReport, ModelOutcome, check_local,
};
use assay_domain::{Event, EventPayload};

use crate::check::validator;
use crate::check_fakes::{
    FakeGates, FakeModel, FakePatches, NoCli, Setup, dirty_tree, unwrap_finding,
};
use crate::plan_fakes::{FakeError, FakePr};

/// 2026-10-13T09:30:00Z, where the fake clock starts.
const START_MILLIS: u64 = 1_791_883_800_000;

/// A sink that keeps every event, on a clock that moves 250 ms each time it is read. It refuses
/// the event at position `refuse_at`, when set.
struct FakeEvents {
    events: Vec<Event>,
    start: SystemTime,
    readings: Cell<u32>,
    refuse_at: Option<usize>,
}

impl FakeEvents {
    fn new() -> Self {
        Self::starting_at(SystemTime::UNIX_EPOCH + Duration::from_millis(START_MILLIS))
    }

    fn starting_at(start: SystemTime) -> Self {
        Self {
            events: Vec::new(),
            start,
            readings: Cell::new(0),
            refuse_at: None,
        }
    }

    fn refusing_at(position: usize) -> Self {
        Self {
            refuse_at: Some(position),
            ..Self::new()
        }
    }

    /// Each event as `(job, idx, kind)`.
    fn listed(&self) -> Vec<(String, u64, String)> {
        self.events
            .iter()
            .map(|event| {
                (
                    event.job.clone(),
                    event.idx,
                    event.payload.kind().code().to_owned(),
                )
            })
            .collect()
    }
}

impl EventSink for FakeEvents {
    type Error = FakeError;

    fn now(&self) -> SystemTime {
        let reading = self.readings.get();
        self.readings.set(reading + 1);
        self.start + Duration::from_millis(u64::from(reading) * 250)
    }

    fn append(&mut self, event: &Event) -> Result<(), FakeError> {
        if self.refuse_at == Some(self.events.len()) {
            return Err(FakeError);
        }
        // The fake keeps its own copy of what it was given.
        self.events.push(event.clone());
        Ok(())
    }
}

/// A local check's result with the fakes' errors.
type Checked = Result<LocalReport, CheckError<FakeError, FakeError, FakeError>>;

/// Checks `pr` with the `unwrap` patch, `gates`, `model` and `events`.
fn check_into(
    pr: &FakePr,
    gates: &mut FakeGates,
    model: &FakeModel,
    events: &mut FakeEvents,
) -> Result<Checked, Box<dyn Error>> {
    let setup = Setup::of(pr)?;
    let patches = FakePatches::lib();
    let ports = CheckPorts {
        objects: pr,
        patches: &patches,
        gates,
        model,
        events,
    };
    Ok(check_local(&setup.request(), ports, &validator()?))
}

/// The `unwrap` finding citing evidence that is not on its line, which validation disposes of.
fn misplaced_finding() -> serde_json::Value {
    let mut finding = unwrap_finding();
    if let Some(evidence) = finding.get_mut("evidence") {
        *evidence = serde_json::json!("this text is not in the file");
    }
    finding
}

/// The stream of a passing `unit` gate and a review that keeps the `unwrap` finding and
/// disposes of a misplaced one.
fn recorded() -> Result<FakeEvents, Box<dyn Error>> {
    let mut events = FakeEvents::new();
    let model = FakeModel::finding(&[unwrap_finding(), misplaced_finding()]);
    check_into(
        &dirty_tree(),
        &mut FakeGates::passing(),
        &model,
        &mut events,
    )??;
    Ok(events)
}

/// The stream as JSONL, one compact envelope per line.
fn jsonl(events: &FakeEvents) -> Result<String, Box<dyn Error>> {
    let mut text = String::new();
    for event in &events.events {
        text.push_str(&serde_json::to_string(event)?);
        text.push('\n');
    }
    Ok(text)
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events/check-local.jsonl")
}

#[test]
fn a_local_run_streams_each_stage_numbered_from_zero_in_each_job() -> Result<(), Box<dyn Error>> {
    let events = recorded()?;

    let expected = [
        ("plan", 0, "PlanComputed"),
        ("gates-test", 0, "GateStarted"),
        ("gates-test", 1, "GateFinished"),
        ("review", 0, "SpecialistStarted"),
        ("review", 1, "SpecialistFinished"),
        ("review", 2, "FindingDisposed"),
        ("review", 3, "FindingRaised"),
        ("decide", 0, "DecisionMade"),
    ]
    .map(|(job, idx, kind)| (job.to_owned(), idx, kind.to_owned()));
    assert_eq!(events.listed(), expected);
    Ok(())
}

#[test]
fn every_event_is_a_valid_assay_event_with_the_runs_key_and_ordered_by_job_and_idx()
-> Result<(), Box<dyn Error>> {
    let events = recorded()?;
    let validator = validator()?;

    let mut next: BTreeMap<&str, u64> = BTreeMap::new();
    for event in &events.events {
        validator.read(Contract::Event, &serde_json::to_vec(event)?)?;
        let expected = next.entry(event.job.as_str()).or_insert(0);
        assert_eq!(
            event.idx, *expected,
            "{} numbers its events in order",
            event.job
        );
        *expected += 1;
    }
    let keys: Vec<&str> = events
        .events
        .iter()
        .map(|event| event.run_key.as_str())
        .collect();
    assert!(
        keys.windows(2).all(|pair| pair.first() == pair.last()) && !keys.is_empty(),
        "one run key: {keys:?}"
    );
    Ok(())
}

#[test]
fn a_gate_reports_its_exit_conclusion_and_duration_and_the_review_its_role_and_cost()
-> Result<(), Box<dyn Error>> {
    let events = recorded()?;

    let data: Vec<serde_json::Value> = events
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.payload,
                EventPayload::GateFinished(_) | EventPayload::SpecialistFinished(_)
            )
        })
        .map(|event| serde_json::to_value(&event.payload))
        .collect::<Result<_, _>>()?;

    assert_eq!(
        data,
        [
            serde_json::json!({"kind": "GateFinished", "data": {
                "plugin": "unit", "exit": 0, "conclusion": "pass", "duration_ms": 250}}),
            serde_json::json!({"kind": "SpecialistFinished", "data": {
                "role": "combined-micro", "cost_aeu": 0.12, "duration_ms": 250}}),
        ]
    );
    Ok(())
}

#[test]
fn the_recorded_stream_is_the_fixture_the_mods_tests_read() -> Result<(), Box<dyn Error>> {
    let produced = jsonl(&recorded()?)?;

    let fixture = fs::read_to_string(fixture_path())?;

    assert_eq!(produced, fixture);
    Ok(())
}

#[test]
fn every_line_of_the_fixture_is_a_valid_assay_event() -> Result<(), Box<dyn Error>> {
    let fixture = fs::read_to_string(fixture_path())?;
    let validator = validator()?;

    let lines: Vec<&str> = fixture.lines().collect();
    for line in &lines {
        validator.read(Contract::Event, line.as_bytes())?;
    }
    assert_eq!(lines.len(), 8);
    Ok(())
}

/// The review's events after `SpecialistStarted`, for a model answering `answers`.
fn review_events(
    answers: impl IntoIterator<Item = Result<ModelOutcome, NoCli>>,
) -> Result<Vec<serde_json::Value>, Box<dyn Error>> {
    let mut events = FakeEvents::new();
    let model = FakeModel::answering(answers);
    check_into(
        &dirty_tree(),
        &mut FakeGates::passing(),
        &model,
        &mut events,
    )??;
    events
        .events
        .iter()
        .filter(|event| event.job == "review" && event.idx > 0)
        .map(|event| Ok(serde_json::to_value(&event.payload)?))
        .collect()
}

#[test]
fn a_completed_review_reports_each_disposition_then_each_kept_finding() -> Result<(), Box<dyn Error>>
{
    let model = FakeModel::finding(&[misplaced_finding(), unwrap_finding(), misplaced_finding()]);
    let mut events = FakeEvents::new();
    check_into(
        &dirty_tree(),
        &mut FakeGates::passing(),
        &model,
        &mut events,
    )??;

    let review: Vec<serde_json::Value> = events
        .events
        .iter()
        .filter(|event| event.job == "review" && event.idx > 0)
        .map(|event| serde_json::to_value(&event.payload))
        .collect::<Result<_, _>>()?;

    let disposed =
        serde_json::json!({"kind": "FindingDisposed", "data": {"code": "evidence-missing"}});
    assert_eq!(
        review,
        [
            serde_json::json!({"kind": "SpecialistFinished", "data": {
                "role": "combined-micro", "cost_aeu": 0.12, "duration_ms": 250}}),
            disposed.clone(),
            disposed,
            serde_json::json!({"kind": "FindingRaised", "data": {}}),
        ]
    );
    Ok(())
}

#[test]
fn a_degraded_review_reports_the_failed_role_and_the_degraded_stage() -> Result<(), Box<dyn Error>>
{
    let events = review_events([Ok(ModelOutcome::Malformed), Ok(ModelOutcome::Malformed)])?;

    assert_eq!(
        events,
        [
            serde_json::json!({"kind": "SpecialistFailed", "data": {
                "role": "combined-micro", "cost_aeu": 0.24, "duration_ms": 250}}),
            serde_json::json!({"kind": "StageDegraded", "data": {
                "stage": "review", "reason": "malformed"}}),
        ]
    );
    Ok(())
}

#[test]
fn a_call_that_exhausts_its_budget_is_reported_as_such() -> Result<(), Box<dyn Error>> {
    let events = review_events([Ok(ModelOutcome::BudgetExhausted)])?;

    assert_eq!(
        events,
        [
            serde_json::json!({"kind": "SpecialistBudgetExhausted", "data": {
                "role": "combined-micro", "cost_aeu": 0.12, "duration_ms": 250}}),
            serde_json::json!({"kind": "StageDegraded", "data": {
                "stage": "review", "reason": "cap"}}),
        ]
    );
    Ok(())
}

#[test]
fn a_review_that_could_not_run_is_reported_unavailable() -> Result<(), Box<dyn Error>> {
    let events = review_events([Err(NoCli)])?;

    let kinds: Vec<&serde_json::Value> = events.iter().map(|event| &event["kind"]).collect();
    assert_eq!(kinds, ["SpecialistFailed", "StageDegraded"]);
    assert_eq!(
        events.last().map(|event| &event["data"]),
        Some(&serde_json::json!({"stage": "review", "reason": "unavailable"}))
    );
    Ok(())
}

#[test]
fn a_gate_that_does_not_run_locally_finishes_without_an_exit_or_a_conclusion()
-> Result<(), Box<dyn Error>> {
    let mut events = FakeEvents::new();
    let model = FakeModel::finding(&[]);

    check_into(
        &dirty_tree(),
        &mut FakeGates::default(),
        &model,
        &mut events,
    )??;

    let finished: Vec<serde_json::Value> = events
        .events
        .iter()
        .filter(|event| matches!(event.payload, EventPayload::GateFinished(_)))
        .map(|event| serde_json::to_value(&event.payload))
        .collect::<Result<_, _>>()?;
    assert_eq!(
        finished,
        [serde_json::json!({"kind": "GateFinished", "data": {
            "plugin": "unit", "duration_ms": 250}})]
    );
    Ok(())
}

#[test]
fn a_refused_event_stops_the_check_before_the_gate_runs() -> Result<(), Box<dyn Error>> {
    let mut events = FakeEvents::refusing_at(1);
    let mut gates = FakeGates::passing();
    let model = FakeModel::finding(&[]);

    let result = check_into(&dirty_tree(), &mut gates, &model, &mut events)?;

    assert!(
        matches!(result, Err(CheckError::Events(FakeError))),
        "{result:?}"
    );
    assert_eq!(gates.calls, [], "GateStarted came first, and was refused");
    assert_eq!(model.call_count(), 0);
    Ok(())
}

#[test]
fn a_refused_event_stops_the_check_before_the_model_is_called() -> Result<(), Box<dyn Error>> {
    let mut events = FakeEvents::refusing_at(3);
    let model = FakeModel::finding(&[]);

    let result = check_into(
        &dirty_tree(),
        &mut FakeGates::passing(),
        &model,
        &mut events,
    )?;

    assert!(
        matches!(result, Err(CheckError::Events(FakeError))),
        "{result:?}"
    );
    assert_eq!(model.call_count(), 0, "SpecialistStarted was refused");
    Ok(())
}

#[test]
fn a_clock_before_1970_stops_the_check() -> Result<(), Box<dyn Error>> {
    let mut events = FakeEvents::starting_at(SystemTime::UNIX_EPOCH - Duration::from_secs(1));
    let model = FakeModel::finding(&[]);

    let result = check_into(
        &dirty_tree(),
        &mut FakeGates::passing(),
        &model,
        &mut events,
    )?;

    assert!(matches!(result, Err(CheckError::Clock)), "{result:?}");
    assert_eq!(events.events, []);
    Ok(())
}
