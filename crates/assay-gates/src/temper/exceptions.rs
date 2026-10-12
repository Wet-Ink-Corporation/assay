//! Base `.assay/exceptions.toml`: the only way a temper `ask` exception or a gate-config change
//! is accepted (`docs/architecture/03-contracts.md` §1, §2.1).
//!
//! ```toml
//! [[exception]]                 # one `#[expect(lint, reason = ..)]` lint in one file
//! path = "crates/x/src/lib.rs"
//! lint = "clippy::too_many_lines"
//! reason = "why the lint does not apply"
//!
//! [[gate_config]]               # one `.claude/temper.json` key, admitted at exactly this value
//! key = "gate"
//! value = '{"allFeatures": true, "doc": true, "scan": true, "stepTimeoutSec": 1200}'
//! reason = "why the gate changes"
//! ```

use serde::Deserialize;
use serde_json::Value;

use super::gate_config::KEYS;

/// The file's path in a repository; it is read from base only.
pub(super) const PATH: &str = ".assay/exceptions.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDto {
    #[serde(default)]
    exception: Vec<ExceptionDto>,
    #[serde(default)]
    gate_config: Vec<GateConfigDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExceptionDto {
    path: String,
    lint: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateConfigDto {
    key: String,
    value: String,
    reason: String,
}

/// What base admits. An absent file admits nothing, which can only make the gate fail.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Allowlist {
    /// (file, lint) pairs.
    exceptions: Vec<(String, String)>,
    /// (key, exact head value) pairs.
    gate_config: Vec<(String, Value)>,
}

impl Allowlist {
    /// Parses the file. Unknown tables or keys, an empty `reason`, a `key` the gate-config diff
    /// does not compare and a `value` that is not JSON are refused: base is trusted, so the
    /// detail may quote it.
    pub(super) fn parse(text: &str) -> Result<Self, String> {
        let dto: FileDto = toml::from_str(text).map_err(|error| error.to_string())?;
        let exceptions = dto
            .exception
            .into_iter()
            .map(|entry| {
                reasoned(&entry.reason, "exception")?;
                Ok((entry.path, entry.lint))
            })
            .collect::<Result<_, String>>()?;
        let gate_config = dto
            .gate_config
            .into_iter()
            .map(|entry| {
                reasoned(&entry.reason, "gate_config")?;
                if !KEYS.contains(&entry.key.as_str()) {
                    return Err(format!(
                        "gate_config key `{}` is not one the gate-config diff compares",
                        entry.key
                    ));
                }
                let value = serde_json::from_str(&entry.value).map_err(|error| {
                    format!("gate_config `{}` value is not JSON: {error}", entry.key)
                })?;
                Ok((entry.key, value))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            exceptions,
            gate_config,
        })
    }

    /// Whether base admits an `#[expect(lint, ..)]` in `file`.
    pub(super) fn admits_exception(&self, file: &str, lint: &str) -> bool {
        self.exceptions
            .iter()
            .any(|(path, admitted)| path == file && admitted == lint)
    }

    /// Whether base admits `key` changing to exactly `head`.
    pub(super) fn admits_config(&self, key: &str, head: &Value) -> bool {
        self.gate_config
            .iter()
            .any(|(admitted, value)| admitted == key && value == head)
    }
}

/// Refuses an entry of `table` without a reason.
fn reasoned(reason: &str, table: &str) -> Result<(), String> {
    if reason.trim().is_empty() {
        Err(format!("an [[{table}]] entry has an empty reason"))
    } else {
        Ok(())
    }
}
