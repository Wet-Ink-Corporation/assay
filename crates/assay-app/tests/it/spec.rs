//! The closed vocabularies in the schemas match the in-tree specification
//! (`docs/architecture/03-contracts.md`), so the two cannot drift apart.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use assay_domain::Disposition;
use serde_json::Value;

use crate::support::{schema, spec_fields, spec_line, spec_names};

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

#[test]
fn disposition_codes_are_exactly_the_spec_list() -> Result<(), Box<dyn Error>> {
    let line = spec_line("Disposition codes (closed enum")?;
    let (_, list) = line
        .split_once("): ")
        .ok_or("03 §5 disposition-code line has no list")?;
    let (list, _) = list
        .split_once(" (")
        .ok_or("03 §5 disposition-code list has no end")?;
    let spec = spec_names(list);

    let domain: BTreeSet<String> = Disposition::ALL
        .iter()
        .map(|disposition| disposition.code().to_owned())
        .collect();

    assert_eq!(domain, spec, "disposition codes, 03 §5");
    Ok(())
}

/// The kinds whose `data` an `if`/`then` in the event schema closes, with the fields it allows.
fn closed_event_data(schema: &Value) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .ok_or("event schema has no $defs")?;
    let mut closed = BTreeMap::new();
    for (name, def) in defs.iter().filter(|(name, _)| name.starts_with("when")) {
        let kind = def
            .pointer("/if/properties/kind")
            .ok_or_else(|| format!("{name} has no kind condition"))?;
        let kinds: Vec<&str> = match (kind.get("const"), kind.get("enum")) {
            (Some(Value::String(one)), _) => vec![one.as_str()],
            (_, Some(Value::Array(many))) => many.iter().filter_map(Value::as_str).collect(),
            _ => return Err(format!("{name} names no kind")),
        };
        let fields: BTreeSet<String> = def
            .pointer("/then/properties/data/properties")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{name} lists no data fields"))?
            .keys()
            .cloned()
            .collect();
        // Each kind of a grouped condition gets its own copy, as `spec_fields` gives it.
        for kind in kinds {
            closed.insert(kind.to_owned(), fields.clone());
        }
    }
    Ok(closed)
}

#[test]
fn closed_event_data_fields_are_exactly_the_spec_fields() -> Result<(), Box<dyn Error>> {
    let spec = spec_fields(&spec_line("`RunRequested`, `PlanComputed`")?);

    let schema_fields = closed_event_data(&schema("event-1")?)?;

    assert_eq!(schema_fields, spec, "event data fields, 03 §6");
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
