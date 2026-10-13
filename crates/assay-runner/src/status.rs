//! `claude --version` and `claude auth status` (06 §1, #83), read into the application's
//! [`CliStatus`] for the preflight to judge.
//!
//! It refuses rather than models (wi-b5affa): the version output must be the single line
//! `<version> (Claude Code)`, and the auth status one JSON object that repeats no key and holds
//! `loggedIn` (a boolean), `authMethod` and `apiProvider` (strings), the shape Claude Code
//! 2.1.296 prints. Fields the preflight does not read are ignored.

use assay_app::{CliStatus, StatusParts, StatusReadError, parse_json_strict};
use serde_json::{Map, Value};

/// What follows the version on `claude --version`'s line.
const VERSION_SUFFIX: &str = " (Claude Code)";

/// Reads the outputs of `claude --version` (`version`) and `claude auth status` (`auth_status`).
///
/// # Errors
/// [`StatusReadError::Version`] for a version output other than one line
/// `<version> (Claude Code)` (an optional final `\n` aside, the version holding no whitespace),
/// [`StatusReadError::NotJson`] for an auth status that is not one JSON object,
/// [`StatusReadError::RepeatedKey`] for one that repeats a key, [`StatusReadError::Field`] for
/// an absent or mistyped field and [`StatusReadError::Name`] for a name out of bounds.
pub fn read_cli_status(version: &[u8], auth_status: &[u8]) -> Result<CliStatus, StatusReadError> {
    let version = read_version(version).ok_or(StatusReadError::Version)?;
    // A lenient parse first, so the strict one below can refuse only for a repeated key.
    let Ok(Value::Object(_)) = serde_json::from_slice::<Value>(auth_status) else {
        return Err(StatusReadError::NotJson);
    };
    let Ok(Value::Object(auth)) = parse_json_strict(auth_status) else {
        return Err(StatusReadError::RepeatedKey);
    };
    CliStatus::new(StatusParts {
        version: version.to_owned(),
        logged_in: auth
            .get("loggedIn")
            .and_then(Value::as_bool)
            .ok_or(StatusReadError::Field { field: "loggedIn" })?,
        auth_method: string_field(&auth, "authMethod")?,
        api_provider: string_field(&auth, "apiProvider")?,
    })
}

/// The version of a `<version> (Claude Code)` line.
fn read_version(output: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(output).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    let version = line.strip_suffix(VERSION_SUFFIX)?;
    (!version.is_empty() && !version.chars().any(|c| c.is_whitespace() || c.is_control()))
        .then_some(version)
}

fn string_field(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<String, StatusReadError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(StatusReadError::Field { field })
}
