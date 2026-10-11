//! The closed vocabularies in the schemas match the in-tree specification
//! (`docs/architecture/03-contracts.md`), so the two cannot drift apart.

use std::collections::BTreeSet;
use std::error::Error;

use serde_json::Value;

use crate::support::{schema, spec_line, spec_names};

fn enum_at(schema: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    Ok(schema
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("no enum at {pointer}"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

#[test]
fn decision_reason_codes_are_exactly_the_spec_list() -> Result<(), Box<dyn Error>> {
    let line = spec_line("Reason codes (closed enum")?;
    let (_, list) = line
        .split_once("): ")
        .ok_or("03 §5 reason-code line has no list")?;
    let (list, _) = list
        .split_once(". (")
        .ok_or("03 §5 reason-code list has no end")?;
    let spec = spec_names(list);

    let schema_codes = enum_at(&schema("decision-1")?, "/$defs/reasonCode/enum")?;

    assert_eq!(schema_codes, spec, "reason codes, 03 §5");
    Ok(())
}

#[test]
fn event_kinds_are_exactly_the_spec_list() -> Result<(), Box<dyn Error>> {
    let spec = spec_names(&spec_line("`RunRequested`, `PlanComputed`")?);

    let schema_kinds = enum_at(&schema("event-1")?, "/properties/kind/enum")?;

    assert_eq!(schema_kinds, spec, "event kinds, 03 §6");
    Ok(())
}

/// `run_uid` keys leases and ledger records (03 §4); both contracts that carry it agree on its
/// shape.
#[test]
fn run_and_event_schemas_define_the_same_run_uid() -> Result<(), Box<dyn Error>> {
    let run = schema("run-1")?;
    let event = schema("event-1")?;

    let in_run = run.pointer("/$defs/runUid");
    let in_event = event.pointer("/$defs/runUid");

    assert!(in_run.is_some(), "run schema defines $defs/runUid");
    assert_eq!(in_run, in_event);
    Ok(())
}
