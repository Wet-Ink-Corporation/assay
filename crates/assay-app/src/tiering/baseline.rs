//! Whether a change to `.claude/temper-baseline.json` only lowers counts (04 §2.2, 03 §2.2
//! `raised`).

use std::collections::BTreeMap;

use assay_domain::BaselineChange;
use serde::Deserialize;

/// The part of a temper baseline the ratchet reads: version 1 and its count table, crate to
/// lint to count. Other keys (`crates`, `updated`) are not counts and are ignored.
#[derive(Debug, Deserialize)]
struct BaselineDto {
    /// Checked while parsing; never read after.
    #[serde(rename = "temper_baseline")]
    _version: Version,
    totals: BTreeMap<String, BTreeMap<String, u64>>,
}

/// The only baseline version this reads.
#[derive(Debug, Deserialize)]
#[serde(try_from = "u64")]
struct Version;

impl TryFrom<u64> for Version {
    type Error = String;

    fn try_from(version: u64) -> Result<Self, Self::Error> {
        if version == 1 {
            Ok(Self)
        } else {
            Err(format!("temper baseline version {version} is not 1"))
        }
    }
}

/// How the temper baseline changed from `base` to `head` text.
///
/// `LoweredOrUnchanged` only when both sides parse as version 1 and no (crate, lint) count is higher at head, a pair absent at
/// base counting as zero. Anything else is `Other`, which keeps the trust path: a side that is
/// missing (the file was added or removed) or does not parse, or a count that went up.
///
/// # Examples
///
/// ```
/// use assay_app::baseline_change;
/// use assay_domain::BaselineChange;
///
/// let base = r#"{"temper_baseline": 1, "totals": {"app": {"missing_docs": 2}}}"#;
/// let head = r#"{"temper_baseline": 1, "totals": {"app": {"missing_docs": 1}}}"#;
/// assert_eq!(baseline_change(Some(base), Some(head)), BaselineChange::LoweredOrUnchanged);
/// ```
#[must_use]
pub fn baseline_change(base: Option<&str>, head: Option<&str>) -> BaselineChange {
    let parse =
        |text: Option<&str>| text.and_then(|text| serde_json::from_str::<BaselineDto>(text).ok());
    let (Some(base), Some(head)) = (parse(base), parse(head)) else {
        return BaselineChange::Other;
    };
    let base_count = |krate: &str, lint: &str| {
        base.totals
            .get(krate)
            .and_then(|lints| lints.get(lint))
            .copied()
            .unwrap_or(0)
    };
    let raised = head.totals.iter().any(|(krate, lints)| {
        lints
            .iter()
            .any(|(lint, &count)| count > base_count(krate, lint))
    });
    if raised {
        BaselineChange::Other
    } else {
        BaselineChange::LoweredOrUnchanged
    }
}
