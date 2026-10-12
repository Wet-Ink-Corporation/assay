//! The `system/init` message of a stream-json transcript (03 §8), read into the application's
//! [`SessionInit`] and judged there.
//!
//! It refuses rather than models (wi-b5affa): the init must be the first message, appear once,
//! repeat no key in any object, and carry every field the rule reads in the shape Claude Code
//! 2.1.296 writes; a plugin entry must have exactly `name`, `path` and `source`. Fields the rule
//! does not read are ignored.

use assay_app::{
    InitParts, InitPlugin, InitReadError, MAX_BUILTINS, SessionInit, parse_json_strict,
};
use serde_json::{Map, Value};

/// The keys of an `init.plugins` entry, all strings.
const PLUGIN_KEYS: [&str; 3] = ["name", "path", "source"];

/// Reads the `system/init` message that opens `transcript`.
///
/// # Errors
/// [`InitReadError::Missing`] when the first non-blank line is not a `system/init` message,
/// [`InitReadError::Repeated`] when another follows, [`InitReadError::RepeatedKey`] when an
/// object in it repeats a key, [`InitReadError::Field`] or
/// [`InitReadError::Entry`] for a field or list entry of the wrong shape, and
/// [`InitReadError::TooMany`] or [`InitReadError::Name`] for one out of bounds.
pub fn read_session_init(transcript: &[u8]) -> Result<SessionInit, InitReadError> {
    let mut lines = transcript
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.trim_ascii().is_empty());
    let first_line = lines.next().ok_or(InitReadError::Missing)?;
    if init_object(first_line).is_none() {
        return Err(InitReadError::Missing);
    }
    // Any reading of a later line that sees an init counts, a repeated key included.
    if lines.any(|line| init_object(line).is_some()) {
        return Err(InitReadError::Repeated);
    }
    // The first line is an init to a lenient parser, so the strict one can refuse it only for
    // a repeated key.
    let Ok(Value::Object(first)) = parse_json_strict(first_line) else {
        return Err(InitReadError::RepeatedKey);
    };
    SessionInit::new(InitParts {
        cli_version: string_field(&first, "claude_code_version")?,
        tools: strings(&first, "tools")?,
        mcp_servers: entries(&first, "mcp_servers", mcp_server)?,
        plugins: entries(&first, "plugins", plugin)?,
        skills: strings(&first, "skills")?,
        agents: strings(&first, "agents")?,
    })
}

/// `line` as an object, when it is a `system` message with subtype `init`.
fn init_object(line: &[u8]) -> Option<Map<String, Value>> {
    let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(line) else {
        return None;
    };
    let is_init = object.get("type").and_then(Value::as_str) == Some("system")
        && object.get("subtype").and_then(Value::as_str) == Some("init");
    is_init.then_some(object)
}

fn string_field(object: &Map<String, Value>, field: &'static str) -> Result<String, InitReadError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(InitReadError::Field { field })
}

/// The array `field`, each entry read by `read` (`None` for a malformed entry).
fn entries<T>(
    object: &Map<String, Value>,
    field: &'static str,
    read: impl Fn(&Value) -> Option<T>,
) -> Result<Vec<T>, InitReadError> {
    let array = object
        .get(field)
        .and_then(Value::as_array)
        .ok_or(InitReadError::Field { field })?;
    if array.len() > MAX_BUILTINS {
        return Err(InitReadError::TooMany {
            field,
            count: array.len(),
        });
    }
    array
        .iter()
        .enumerate()
        .map(|(index, entry)| read(entry).ok_or(InitReadError::Entry { field, index }))
        .collect()
}

fn strings(object: &Map<String, Value>, field: &'static str) -> Result<Vec<String>, InitReadError> {
    entries(object, field, |entry| entry.as_str().map(str::to_owned))
}

/// An `mcp_servers` entry's `name`; its other keys (`status`) are not read.
fn mcp_server(entry: &Value) -> Option<String> {
    entry.get("name")?.as_str().map(str::to_owned)
}

/// A `plugins` entry with exactly the string keys `name`, `path` and `source`.
fn plugin(entry: &Value) -> Option<InitPlugin> {
    let object = entry.as_object()?;
    if object.len() != PLUGIN_KEYS.len() {
        return None;
    }
    let text = |key| object.get(key)?.as_str().map(str::to_owned);
    Some(InitPlugin {
        name: text("name")?,
        path: text("path")?,
        source: text("source")?,
    })
}
