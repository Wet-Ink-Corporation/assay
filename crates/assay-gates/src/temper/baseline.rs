//! Step 1, `raised`: the totals diff of `.claude/temper-baseline.json` at base and head
//! (`docs/architecture/03-contracts.md` §2.1). temper's `baseline --check` exits 0 on a raise
//! (05 T5), so assay compares the counts itself.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::report::json_detail;

/// The baseline's path in a repository.
pub(super) const PATH: &str = ".claude/temper-baseline.json";

/// The only baseline format this adapter reads (`"temper_baseline": 1`).
const FORMAT: u64 = 1;

/// Counts per crate, then per lint.
type Counts = BTreeMap<String, BTreeMap<String, u64>>;

/// The parts of temper's baseline the totals diff reads; the header fields and `crates` are
/// not compared.
#[derive(Debug, Deserialize)]
struct BaselineDto {
    temper_baseline: u64,
    totals: Counts,
}

/// A baseline's accepted debt: a count per (crate, lint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Totals(Counts);

impl Totals {
    /// Parses baseline JSON. Counts must be non-negative integers; any other shape, or a format
    /// other than 1, is refused with a detail that does not quote the file.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let dto: BaselineDto =
            serde_json::from_slice(bytes).map_err(|error| json_detail(&error))?;
        if dto.temper_baseline != FORMAT {
            return Err(format!(
                "\"temper_baseline\" is not {FORMAT}, the only format this adapter reads"
            ));
        }
        Ok(Self(dto.totals))
    }
}

/// One (crate, lint) count that head records above base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Raise<'a> {
    /// The crate, as the baseline names it.
    pub(super) krate: &'a str,
    /// The lint.
    pub(super) lint: &'a str,
    /// The base count, `None` when base has no such entry.
    pub(super) from: Option<u64>,
    /// The head count.
    pub(super) to: u64,
}

/// Every head entry whose count is above base's, or that base does not have, in crate and lint
/// order. A count that only goes down, or an entry that disappears, is not a raise.
pub(super) fn raises<'a>(base: &Totals, head: &'a Totals) -> Vec<Raise<'a>> {
    head.0
        .iter()
        .flat_map(|(krate, lints)| lints.iter().map(move |(lint, to)| (krate, lint, *to)))
        .filter_map(|(krate, lint, to)| {
            let from = base.0.get(krate).and_then(|lints| lints.get(lint)).copied();
            let raised = from.is_none_or(|from| to > from);
            raised.then_some(Raise {
                krate,
                lint,
                from,
                to,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(json: &str) -> Totals {
        Totals::parse(json.as_bytes()).expect("a valid baseline")
    }

    #[test]
    fn a_new_entry_counts_as_a_raise_even_at_zero() {
        let base = totals(r#"{"temper_baseline": 1, "totals": {}}"#);
        let head = totals(r#"{"temper_baseline": 1, "totals": {"c": {"l": 0}}}"#);

        assert_eq!(
            raises(&base, &head),
            [Raise {
                krate: "c",
                lint: "l",
                from: None,
                to: 0
            }],
            "any new entry fails (03 §2.1)"
        );
    }

    #[test]
    fn an_equal_count_is_not_a_raise() {
        let both = totals(r#"{"temper_baseline": 1, "totals": {"c": {"l": 3}}}"#);

        assert_eq!(raises(&both, &both), [], "nothing moved");
    }
}
