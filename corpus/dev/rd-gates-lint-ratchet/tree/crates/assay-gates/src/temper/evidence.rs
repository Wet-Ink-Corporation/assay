//! Step 6: the evidence of a `gates-lint` finding is the line at its `file:line`, read by the
//! adapter from the head commit, or from base when head does not hold that line
//! (`docs/architecture/03-contracts.md` §2.1). Never temper's own text, and never the checkout
//! on disk, which a build script could have rewritten.
//!
//! Each side's tree is listed once and the cited files are read in one batch, within a byte
//! budget charged by listed size; a file past it keeps a synthesised quote.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU64;

use assay_domain::Side;

use super::report::{Spot, line_evidence};
use crate::revision::{EntryKind, Revision, RevisionFiles, TreeEntry};

/// Most bytes of cited files read for evidence in one run (H-13).
pub(super) const MAX_EVIDENCE_BYTES: u64 = 16 << 20;

/// The quoted lines found, by (file, line).
#[derive(Debug, Default)]
pub(super) struct Lines<'w>(HashMap<(&'w str, NonZeroU64), (Side, String)>);

impl Lines<'_> {
    /// Where a finding at `file:line` sits: its line's text on the side that holds it, or
    /// `fallback` on head when no side does.
    pub(super) fn spot(&self, file: &str, line: NonZeroU64, fallback: &str) -> Spot {
        match self.0.get(&(file, line)) {
            Some((side, evidence)) => Spot {
                file: file.to_owned(),
                line,
                side: *side,
                evidence: evidence.to_owned(),
            },
            None => Spot::at(file.to_owned(), line, None, fallback),
        }
    }
}

/// Reads the lines `wanted` cites: head first, then base for the lines head does not hold.
///
/// # Errors
///
/// The side whose tree or blobs could not be read, with the reader's error.
pub(super) fn gather<'w, R: RevisionFiles>(
    revisions: &R,
    wanted: impl IntoIterator<Item = (&'w str, NonZeroU64)>,
    budget: u64,
) -> Result<Lines<'w>, (Revision, R::Error)> {
    let mut pending: BTreeMap<&'w str, BTreeSet<NonZeroU64>> = BTreeMap::new();
    for (file, line) in wanted {
        pending.entry(file).or_default().insert(line);
    }
    let mut lines = Lines::default();
    let mut left = budget;
    for (revision, side) in [(Revision::Head, Side::Right), (Revision::Base, Side::Left)] {
        if pending.is_empty() {
            break;
        }
        let failed = |error| (revision, error);
        let entries = revisions.list(revision, None).map_err(failed)?;
        let cited = entries.iter().filter(|entry| {
            entry.kind == EntryKind::File && pending.contains_key(entry.path.as_str())
        });
        let chosen = within_budget(cited, &mut left);
        let blobs = revisions.read(&chosen).map_err(failed)?;
        for (entry, bytes) in chosen.iter().zip(blobs) {
            let Some((file, needed)) = pending.remove_entry(entry.path.as_str()) else {
                continue;
            };
            let unread = quote_into(&mut lines, (file, side), needed, &bytes);
            if !unread.is_empty() {
                pending.insert(file, unread);
            }
        }
    }
    Ok(lines)
}

/// The entries of `cited` whose listed size fits in what is `left` of the budget, in order;
/// each one taken is charged.
fn within_budget<'e>(
    cited: impl Iterator<Item = &'e TreeEntry>,
    left: &mut u64,
) -> Vec<&'e TreeEntry> {
    cited
        .filter(|entry| {
            let Some(rest) = entry.size.and_then(|size| left.checked_sub(size)) else {
                return false;
            };
            *left = rest;
            true
        })
        .collect()
}

/// Quotes the `needed` lines of `bytes` into `lines` for `file` on `side`, and returns those
/// past its end. A line that is there but blank is quoted by nobody: the finding falls back.
fn quote_into<'w>(
    lines: &mut Lines<'w>,
    (file, side): (&'w str, Side),
    mut needed: BTreeSet<NonZeroU64>,
    bytes: &[u8],
) -> BTreeSet<NonZeroU64> {
    let text = String::from_utf8_lossy(bytes);
    for (index, line) in text.lines().enumerate() {
        let number = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU64::new);
        let Some(number) = number else {
            break;
        };
        if needed.remove(&number)
            && let Some(evidence) = line_evidence(line)
        {
            lines.0.insert((file, number), (side, evidence));
        }
        if needed.is_empty() {
            break;
        }
    }
    needed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            kind: EntryKind::File,
            object: "0".repeat(40),
            size: Some(size),
        }
    }

    #[test]
    fn takes_files_up_to_exactly_the_budget_and_skips_one_past_it() {
        let entries = [entry("a", 4), entry("b", 6), entry("c", 1)];
        let mut left = 10;

        let chosen: Vec<&str> = within_budget(entries.iter(), &mut left)
            .into_iter()
            .map(|entry| entry.path.as_str())
            .collect();

        assert_eq!(
            (chosen, left),
            (vec!["a", "b"], 0),
            "c is one byte past the budget"
        );
    }

    #[test]
    fn quotes_only_the_lines_asked_for_and_returns_those_past_the_end() {
        let mut lines = Lines::default();
        let needed: BTreeSet<NonZeroU64> =
            [1, 3, 9].into_iter().filter_map(NonZeroU64::new).collect();

        let unread = quote_into(
            &mut lines,
            ("f.rs", Side::Right),
            needed,
            b"one\n\n  three  \n",
        );

        let three = NonZeroU64::new(3).map(|line| lines.spot("f.rs", line, "x").evidence);
        assert_eq!(
            (
                unread.into_iter().map(NonZeroU64::get).collect::<Vec<_>>(),
                three
            ),
            (vec![9], Some("three".to_owned()))
        );
    }
}
