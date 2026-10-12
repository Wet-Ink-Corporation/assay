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
//!
//! [[test_inventory]]            # every test-inventory loss of one file, between two blobs
//! path = "crates/x/tests/it.rs"
//! base = "<blob id at base>"    # omitted when the file is absent at base
//! head = "<blob id at head>"    # omitted when the file is absent at head
//! reason = "why the tests go"
//!
//! [[manifest_switch]]           # every manifest switch of one `Cargo.toml`, between two blobs
//! path = "crates/x/Cargo.toml"
//! base = "<blob id at base>"
//! head = "<blob id at head>"
//! reason = "why the switch changes"
//! ```
//!
//! A `gate_config` entry without `base` admits adding the key; none admits removing one. A
//! `test_inventory` or `manifest_switch` entry binds the exact pair of blobs, so it admits that
//! one change and cannot be replayed for another.

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
    #[serde(default)]
    test_inventory: Vec<BlobChangeDto>,
    #[serde(default)]
    manifest_switch: Vec<BlobChangeDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlobChangeDto {
    path: String,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    head: Option<String>,
    reason: String,
}

/// One admitted change of one file: its path and its exact blob ids at base and head (`None`:
/// absent on that side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlobChange {
    path: String,
    base: Option<String>,
    head: Option<String>,
}

impl BlobChange {
    /// Whether this is the change of `path` from blob `base` to blob `head`.
    fn is(&self, path: &str, base: Option<&str>, head: Option<&str>) -> bool {
        self.path == path && self.base.as_deref() == base && self.head.as_deref() == head
    }

    /// Validates one `table` entry: a reason, at least one side, and git object ids.
    fn parse(entry: BlobChangeDto, table: &str) -> Result<Self, String> {
        reasoned(&entry.reason, table)?;
        if entry.base.is_none() && entry.head.is_none() {
            return Err(format!(
                "a [[{table}]] entry for `{}` names neither a base nor a head blob",
                entry.path
            ));
        }
        for id in [&entry.base, &entry.head].into_iter().flatten() {
            if !is_object_id(id) {
                return Err(format!(
                    "a [[{table}]] entry for `{}` has `{id}`, not a git object id",
                    entry.path
                ));
            }
        }
        Ok(Self {
            path: entry.path,
            base: entry.base,
            head: entry.head,
        })
    }
}

/// Whether `id` is a git object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
fn is_object_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    test_inventory: Vec<BlobChange>,
    manifest_switch: Vec<BlobChange>,
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
        let blobs = |entries: Vec<BlobChangeDto>, table: &str| {
            entries
                .into_iter()
                .map(|entry| BlobChange::parse(entry, table))
                .collect::<Result<Vec<_>, String>>()
        };
        Ok(Self {
            exceptions,
            gate_config,
            test_inventory: blobs(dto.test_inventory, "test_inventory")?,
            manifest_switch: blobs(dto.manifest_switch, "manifest_switch")?,
        })
    }

    /// Whether base admits the test-inventory losses of `path` changing from blob `base` to
    /// blob `head` (`None`: absent).
    pub(super) fn admits_inventory(
        &self,
        path: &str,
        base: Option<&str>,
        head: Option<&str>,
    ) -> bool {
        self.test_inventory
            .iter()
            .any(|change| change.is(path, base, head))
    }

    /// Whether base admits the manifest switches of `path` changing from blob `base` to blob
    /// `head` (`None`: absent).
    pub(super) fn admits_switch(&self, path: &str, base: Option<&str>, head: Option<&str>) -> bool {
        self.manifest_switch
            .iter()
            .any(|change| change.is(path, base, head))
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
