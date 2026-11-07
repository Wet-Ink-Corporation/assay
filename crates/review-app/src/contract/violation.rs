//! Turns a schema validation error into a [`SchemaViolation`] that quotes nothing from the
//! document: the document is untrusted, and so are its values and its object keys.

use std::borrow::Cow;
use std::collections::BTreeSet;

use jsonschema::ValidationError;
use jsonschema::error::ValidationErrorKind;
use serde_json::Value;

use super::SchemaViolation;

/// Stands in for an object key the contract's schema does not name.
const MASK: &str = "*";

/// The property names a schema declares: every key of every `properties` map in it. Only these
/// may appear in a reported path.
pub(super) fn declared_names(schema: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_names(schema, &mut names);
    names
}

fn collect_names(schema: &Value, names: &mut BTreeSet<String>) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                names.extend(properties.keys().cloned());
            }
            for value in map.values() {
                collect_names(value, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_names(item, names);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// The violation `error` reports on `document`, with every undeclared key masked.
pub(super) fn violation(
    error: &ValidationError<'_>,
    document: &Value,
    declared: &BTreeSet<String>,
) -> SchemaViolation {
    SchemaViolation {
        instance_path: masked_path(error.instance_path().as_str(), document, declared),
        keyword: error.kind().keyword().to_owned(),
        message: describe(error),
    }
}

/// jsonschema's masked text replaces the rejected value but still lists unexpected property
/// names, so those kinds report a count instead. Every other kind's masked text quotes only the
/// schema (limits, patterns, enum options, required names).
fn describe(error: &ValidationError<'_>) -> String {
    match error.kind() {
        ValidationErrorKind::AdditionalProperties { unexpected }
        | ValidationErrorKind::UnevaluatedProperties { unexpected } => match unexpected.len() {
            1 => "1 unexpected property is not allowed".to_owned(),
            count => format!("{count} unexpected properties are not allowed"),
        },
        ValidationErrorKind::PropertyNames { .. } => "a property name is not allowed".to_owned(),
        _ => error.masked().to_string(),
    }
}

/// Rebuilds the JSON Pointer `pointer` by walking `document`: array indices are kept, object
/// keys only when `declared` names them, and anything else becomes [`MASK`].
fn masked_path(pointer: &str, document: &Value, declared: &BTreeSet<String>) -> String {
    let mut masked = String::with_capacity(pointer.len());
    let mut at = Some(document);
    for raw in pointer.split('/').skip(1) {
        let (shown, next) = match at {
            Some(Value::Array(items)) => match raw.parse::<usize>() {
                Ok(index) => (raw, items.get(index)),
                Err(_) => (MASK, None),
            },
            Some(Value::Object(map)) => {
                let key = unescape(raw);
                let shown = if declared.contains(key.as_ref()) {
                    raw
                } else {
                    MASK
                };
                (shown, map.get(key.as_ref()))
            }
            _ => (MASK, None),
        };
        masked.push('/');
        masked.push_str(shown);
        at = next;
    }
    masked
}

/// Undoes RFC 6901 escaping: `~1` is `/`, then `~0` is `~`.
fn unescape(segment: &str) -> Cow<'_, str> {
    if segment.contains('~') {
        Cow::Owned(segment.replace("~1", "/").replace("~0", "~"))
    } else {
        Cow::Borrowed(segment)
    }
}
