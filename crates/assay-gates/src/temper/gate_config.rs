//! Step 3, the gate-config diff: `.claude/temper.json` compared key by key at base and head
//! (`docs/architecture/03-contracts.md` §2.1).

use serde_json::{Map, Value};

use super::exceptions::Allowlist;
use super::report::json_detail;

/// The config's path in a repository.
pub(super) const PATH: &str = ".claude/temper.json";

/// The keys compared: temper's `GATE_CONFIG_KEYS` (`gate`, `scanner`, `manifests`), which the
/// gate reads, then the keys that steer temper's hooks, compared as defence in depth.
pub(super) const KEYS: [&str; 8] = [
    "gate",
    "scanner",
    "manifests",
    "protected",
    "review",
    "skillGate",
    "preapprovedDependencies",
    "askOnNewDependency",
];

/// A parsed `.claude/temper.json`: a JSON object. As with temper's `JSON.parse`, a repeated
/// key keeps its last value.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TemperConfig(Map<String, Value>);

impl TemperConfig {
    /// Parses the file; anything but a JSON object is refused with a detail that does not quote
    /// it.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, String> {
        match serde_json::from_slice(bytes).map_err(|error| json_detail(&error))? {
            Value::Object(map) => Ok(Self(map)),
            _ => Err("not a JSON object".to_owned()),
        }
    }
}

/// Every compared key whose value differs between `base` and `head` (present on one side only
/// included), in [`KEYS`] order, unless `allowlist` admits exactly this base value changing to
/// exactly this head value. A removed key is never admitted.
pub(super) fn drifted(
    base: &TemperConfig,
    head: &TemperConfig,
    allowlist: &Allowlist,
) -> Vec<&'static str> {
    KEYS.into_iter()
        .filter(|key| {
            let (before, after) = (base.0.get(*key), head.0.get(*key));
            before != after
                && after.is_none_or(|value| !allowlist.admits_config(key, before, value))
        })
        .collect()
}
