//! Base `.assay/exceptions.toml`: the only way a temper `ask` exception or a gate-config change
//! is accepted (`docs/architecture/03-contracts.md` §1, §2.1).
//!
//! ```toml
//! [[exception]]                 # one `#[expect(lint, reason = ..)]` lint in one file
//! path = "crates/x/src/lib.rs"
//! lint = "clippy::too_many_lines"
//! reason = "why the lint does not apply"
//!
//! [[gate_config]]               # one `.claude/temper.json` key, from exactly `base` to `head`
//! key = "gate"
//! base = '{"allFeatures": true, "doc": true, "scan": true, "stepTimeoutSec": 900}'
//! head = '{"allFeatures": true, "doc": true, "scan": true, "stepTimeoutSec": 1200}'
//! reason = "why the gate changes"
//! ```
//!
//! A `gate_config` entry without `base` admits adding the key; none admits removing one.

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
    #[serde(default)]
    base: Option<String>,
    head: String,
    reason: String,
}

/// One admitted gate-config change: the key, its base value (`None`: absent at base) and its
/// head value.
#[derive(Debug, Clone, PartialEq)]
struct ConfigChange {
    key: String,
    base: Option<Value>,
    head: Value,
}

/// What base admits. An absent file admits nothing, which can only make the gate fail.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Allowlist {
    /// (file, lint) pairs.
    exceptions: Vec<(String, String)>,
    gate_config: Vec<ConfigChange>,
}

impl Allowlist {
    /// Parses the file. Unknown tables or keys, an empty `reason`, a `key` the gate-config diff
    /// does not compare and a `base` or `head` that is not JSON are refused: base is trusted,
    /// so the detail may quote it.
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
            .map(ConfigChange::try_from)
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

    /// Whether base admits `key` changing from exactly `base` (`None`: absent) to exactly
    /// `head`.
    pub(super) fn admits_config(&self, key: &str, base: Option<&Value>, head: &Value) -> bool {
        self.gate_config
            .iter()
            .any(|change| change.key == key && change.base.as_ref() == base && change.head == *head)
    }
}

impl TryFrom<GateConfigDto> for ConfigChange {
    type Error = String;

    fn try_from(entry: GateConfigDto) -> Result<Self, String> {
        reasoned(&entry.reason, "gate_config")?;
        if !KEYS.contains(&entry.key.as_str()) {
            return Err(format!(
                "gate_config key `{}` is not one the gate-config diff compares",
                entry.key
            ));
        }
        let json = |side: &str, text: &str| {
            serde_json::from_str(text)
                .map_err(|error| format!("gate_config `{}` {side} is not JSON: {error}", entry.key))
        };
        let base = entry
            .base
            .as_deref()
            .map(|text| json("base", text))
            .transpose()?;
        let head = json("head", &entry.head)?;
        Ok(Self {
            key: entry.key,
            base,
            head,
        })
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
