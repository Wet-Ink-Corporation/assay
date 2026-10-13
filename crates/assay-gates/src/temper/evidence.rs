//! Step 6: the evidence of a `gates-lint` finding is the line at its `file:line`, read by the
//! adapter from the head commit, or from base when head does not hold the file at all (a
//! deleted line, `docs/architecture/03-contracts.md` §2.1). Never temper's own text, and never
//! the checkout on disk, which a build script could have rewritten.
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

/// Reads the lines `wanted` cites: head first, then base for the files head does not hold.
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
        let present: Vec<&TreeEntry> = entries
            .iter()
            .filter(|entry| pending.contains_key(entry.path.as_str()))
            .collect();
        let files = present
            .iter()
            .copied()
            .filter(|entry| entry.kind == EntryKind::File);
        let chosen = within_budget(files, &mut left);
        let blobs = revisions.read(&chosen).map_err(failed)?;
        for (entry, bytes) in chosen.iter().zip(blobs) {
            if let Some((file, needed)) = pending.get_key_value(entry.path.as_str()) {
                quote_into(&mut lines, (file, side), needed, &bytes);
            }
        }
        // A file this side holds is answered here, quoted or not: base is read only for the
        // files head does not hold at all (03 §2.1 step 6, deleted lines).
        for entry in present {
            pending.remove(entry.path.as_str());
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

/// Quotes the `needed` lines of `bytes` into `lines` for `file` on `side`, in one pass that
/// stops at the last one. A line past the end, or there but blank, is quoted by nobody: its
/// finding falls back.
fn quote_into<'w>(
    lines: &mut Lines<'w>,
    (file, side): (&'w str, Side),
    needed: &BTreeSet<NonZeroU64>,
    bytes: &[u8],
) {
    let Some(&last) = needed.last() else {
        return;
    };
    let text = String::from_utf8_lossy(bytes);
    for (index, line) in text.lines().enumerate() {
        let number = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU64::new);
        let Some(number) = number else {
            break;
        };
        if needed.contains(&number)
            && let Some(evidence) = line_evidence(line)
        {
            lines.0.insert((file, number), (side, evidence));
        }
        if number >= last {
            break;
        }
    }
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
    fn quotes_only_the_lines_asked_for_and_none_past_the_end() {
        let mut lines = Lines::default();
        let needed: BTreeSet<NonZeroU64> =
            [1, 3, 9].into_iter().filter_map(NonZeroU64::new).collect();

        quote_into(
            &mut lines,
            ("f.rs", Side::Right),
            &needed,
            b"one\ntwo\n  three  \n",
        );

        let mut quoted: Vec<(u64, String)> = lines
            .0
            .into_iter()
            .map(|((_, line), (_, evidence))| (line.get(), evidence))
            .collect();
        quoted.sort();
        assert_eq!(
            quoted,
            [(1, "one".to_owned()), (3, "three".to_owned())],
            "line 2 was not asked for and line 9 is past the end"
        );
    }
}
