//! `assay check --local --events <path|->` and `assay status --local` (#130) end to end on the
//! fixture repository of `check.rs`: the event stream on stdout or in a new file, every line a
//! valid `assay.event/1` numbered per job; an existing path refused; and the local status
//! missing, fresh and stale.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;

use assay_app::{Contract, ContractValidator};
use serde_json::Value;

use crate::check::{Scene, snapshot_tree, write};

/// The terminal report's first line.
const BANNER: &str = "assay check --local: advisory, CI never reads this report\n";

/// The kinds of a passing `unit` gate and a completed review with no finding.
const KINDS: [&str; 6] = [
    "PlanComputed",
    "GateStarted",
    "GateFinished",
    "SpecialistStarted",
    "SpecialistFinished",
    "DecisionMade",
];

/// Each line of `stream`'s kind, after checking it is a valid `assay.event/1` and that each
/// job numbers its events 0, 1, 2, ... in order.
fn kinds(stream: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let mut next: BTreeMap<String, u64> = BTreeMap::new();
    let mut kinds = Vec::new();
    for line in stream.lines() {
        let event = validator.read(Contract::Event, line.as_bytes())?;
        let text = |field: &str| event.get(field).and_then(Value::as_str).map(str::to_owned);
        let job = text("job").ok_or("job is text")?;
        let expected = next.entry(job).or_insert(0);
        assert_eq!(
            event.get("idx").and_then(Value::as_u64),
            Some(*expected),
            "{line}"
        );
        *expected += 1;
        kinds.push(text("kind").ok_or("kind is text")?);
    }
    Ok(kinds)
}

#[test]
fn events_stream_on_stdout_and_the_report_moves_to_stderr() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let outcome = scene.check(&["--local", "--claude", &scene.claude()?, "--events", "-"])?;

    assert_eq!(kinds(&outcome.stdout)?, KINDS, "{outcome:?}");
    assert!(outcome.stderr.starts_with(BANNER), "{outcome:?}");
    assert_eq!(outcome.code, Some(1), "the full-tier decision still fails");
    Ok(())
}

#[test]
fn events_go_to_a_new_file_and_the_report_stays_on_stdout() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let stream = scene.path("events.jsonl");
    let stream_arg = stream.to_str().ok_or("scratch path is not UTF-8")?;

    let outcome = scene.check(&[
        "--local",
        "--claude",
        &scene.claude()?,
        "--events",
        stream_arg,
    ])?;

    assert_eq!(kinds(&fs::read_to_string(&stream)?)?, KINDS, "{outcome:?}");
    assert!(outcome.stdout.starts_with(BANNER), "{outcome:?}");
    Ok(())
}

#[test]
fn an_existing_events_path_is_refused_and_left_as_it_was() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let stream = scene.path("events.jsonl");
    fs::write(&stream, "kept\n")?;
    let stream_arg = stream.to_str().ok_or("scratch path is not UTF-8")?;

    let outcome = scene.check(&[
        "--local",
        "--claude",
        &scene.claude()?,
        "--events",
        stream_arg,
    ])?;

    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (
            Some(2),
            format!(
                "assay: {stream_arg} already exists: --events writes a new file, or - for stdout\n"
            )
            .as_str()
        )
    );
    assert_eq!(fs::read_to_string(&stream)?, "kept\n");
    assert!(
        !scene.path("work/.git/assay").exists(),
        "nothing was checked"
    );
    Ok(())
}

#[test]
fn an_events_value_that_is_missing_empty_or_a_flag_is_a_usage_error() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;
    let usage = "assay: usage: assay check --local [--base <branch>] [--claude <program>], run \
                 from the repository root\n";

    for args in [
        &["--local", "--events"][..],
        &["--local", "--events", ""],
        &["--local", "--events", "--base", "main"],
        &["--local", "--events", "-", "--events", "-"],
    ] {
        let outcome = scene.check(args)?;
        assert_eq!(
            (outcome.code, outcome.stderr.as_str()),
            (Some(2), usage),
            "{args:?}"
        );
    }
    Ok(())
}

/// `assay status --json --local` in the scene's working tree, checked against
/// `assay.status/1`.
fn local_status(scene: &Scene) -> Result<Value, Box<dyn Error>> {
    let outcome = scene.status(&["--json", "--local"])?;
    assert_eq!(outcome.code, Some(0), "{outcome:?}");
    Ok(ContractValidator::new()?.read(Contract::Status, outcome.stdout.as_bytes())?)
}

#[test]
fn before_any_check_the_local_status_has_no_result() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let tree = snapshot_tree(&scene.path("work"))?;

    let status = local_status(&scene)?;

    assert_eq!(
        (
            &status["source"],
            &status["tree"],
            &status["local_tree"],
            &status["stale"],
            &status["check"],
        ),
        (
            &Value::from("local"),
            &Value::Null,
            &Value::from(tree),
            &Value::from(false),
            &Value::Null,
        )
    );
    Ok(())
}

#[test]
fn after_a_check_the_local_status_is_fresh_and_after_an_edit_stale() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let work = scene.path("work");
    scene.check(&["--local", "--claude", &scene.claude()?])?;
    let checked = snapshot_tree(&work)?;

    let fresh = local_status(&scene)?;
    write(&work, "src/lib.rs", "pub fn one() -> u32 {\n    3\n}\n")?;
    let edited = snapshot_tree(&work)?;
    let stale = local_status(&scene)?;

    assert_eq!(
        (&fresh["tree"], &fresh["local_tree"], &fresh["stale"]),
        (
            &Value::from(checked.as_str()),
            &Value::from(checked.as_str()),
            &Value::from(false)
        )
    );
    assert_eq!(fresh["check"]["status"], "completed");
    assert_eq!(
        (&stale["tree"], &stale["local_tree"], &stale["stale"]),
        (
            &Value::from(checked),
            &Value::from(edited),
            &Value::from(true)
        )
    );
    assert_eq!(stale["decision"], fresh["decision"]);
    Ok(())
}

#[test]
fn the_local_status_as_text_says_it_is_advisory() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let outcome = scene.status(&["--local"])?;

    assert_eq!(outcome.code, Some(0), "{outcome:?}");
    assert!(
        outcome
            .stdout
            .starts_with("assay status --local: advisory, CI never reads it\n"),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
fn the_local_status_refuses_a_prs_arguments() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let outcome = scene.status(&["--local", "--pr", "3"])?;

    assert_eq!(outcome.code, Some(2), "{outcome:?}");
    assert!(
        outcome
            .stderr
            .starts_with("assay: --pr does not go with --local; usage: "),
        "{outcome:?}"
    );
    Ok(())
}
