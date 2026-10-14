//! Whether a change to `.claude/temper-baseline.json` only lowers counts (04 §2.2, 03 §2.2
//! `raised`).

use std::collections::BTreeMap;

use assay_domain::BaselineChange;
use serde::Deserialize;
use serde_json::{Map, Value};

/// The count table: crate to lint to count.
type Totals = BTreeMap<String, BTreeMap<String, u64>>;

/// The keys a tightening may change: the counts themselves, and the time they were updated.
/// Every other key (`temper_baseline`, `crates`, `commit`, anything temper adds later) must be
/// equal, because it decides what the counts allow, as `crates` maps them to directories.
const MAY_CHANGE: [&str; 2] = ["totals", "updated"];

/// One side of the baseline: its whole top-level object and its parsed counts.
#[derive(Debug)]
struct Baseline {
    document: Map<String, Value>,
    totals: Totals,
}

impl Baseline {
    /// A version 1 baseline whose `totals` are counts, or `None`.
    fn parse(text: &str) -> Option<Self> {
        let document: Map<String, Value> = serde_json::from_str(text).ok()?;
        if document.get("temper_baseline").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let totals = Totals::deserialize(document.get("totals")?).ok()?;
        Some(Self { document, totals })
    }

    /// The top-level entries a tightening may not change.
    fn fixed(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.document
            .iter()
            .filter(|(key, _)| !MAY_CHANGE.contains(&key.as_str()))
    }

    fn count(&self, krate: &str, lint: &str) -> u64 {
        self.totals
            .get(krate)
            .and_then(|lints| lints.get(lint))
            .copied()
            .unwrap_or(0)
    }
}

/// How the temper baseline changed from `base` to `head` text.
///
/// `LoweredOrUnchanged` only when both sides parse as version 1, every top-level key other than
/// `totals` and `updated` is equal (so `crates` and `commit` cannot move an allowance), and no
/// (crate, lint) count is higher at head, a pair absent at base counting as zero. Anything else
/// is `Other`, which keeps the trust path: a side that is missing (the file was added or
/// removed) or does not parse, a changed fixed key, or a count that went up.
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
    let (Some(base), Some(head)) = (
        base.and_then(Baseline::parse),
        head.and_then(Baseline::parse),
    ) else {
        return BaselineChange::Other;
    };
    let fixed_equal = base.fixed().eq(head.fixed());
    let raised = head.totals.iter().any(|(krate, lints)| {
        lints
            .iter()
            .any(|(lint, &count)| count > base.count(krate, lint))
    });
    if fixed_equal && !raised {
        BaselineChange::LoweredOrUnchanged
    } else {
        BaselineChange::Other
    }
}
