//! Evidence location, 04 §4 step 2: where a normalised evidence span occurs in a whole file,
//! and which occurrence a drifted line moves to.

use assay_domain::normalise;

/// The normaliser's wildcard, which `...` and `…` both become (03 §3).
const WILDCARD: char = '\u{2026}';

/// The most lines one match may span, so a wildcard cannot stretch across a whole file.
pub(super) const MAX_SPAN_LINES: u64 = 50;

/// The lines (1-based, ascending, distinct) on which `evidence` starts in `text`.
///
/// Both sides are normalised with the shared normaliser, `text` line by line and joined with
/// single spaces, so a span quoted across lines matches as it would in one string. The pieces of
/// the evidence between wildcards must occur in order, the first starting on the match line, and
/// the whole match must end within [`MAX_SPAN_LINES`] lines of it. Evidence without content (only
/// blanks and wildcards) matches nowhere.
///
/// Linear in the file for any evidence: each later piece is matched at its first occurrence
/// after the previous piece, and since starts only move forward so do those occurrences, so each
/// is searched for once and reused while it still lies ahead.
pub(super) fn find(text: &str, evidence: &str) -> Vec<u64> {
    let normalised = normalise(evidence);
    let pieces: Vec<&str> = normalised
        .split(WILDCARD)
        .filter(|piece| !piece.is_empty())
        .collect();
    let Some((first, rest)) = pieces.split_first() else {
        return Vec::new();
    };
    let joined = Joined::of(text);
    let mut lines: Vec<u64> = Vec::new();
    let mut found: Vec<Option<usize>> = vec![None; rest.len()];
    let mut from = 0;
    while let Some(start) = joined.find(first, from) {
        let mut end = Some(start.saturating_add(first.len()));
        for (piece, last) in rest.iter().zip(found.iter_mut()) {
            end = end.and_then(|needed| {
                let at = match *last {
                    Some(at) if at >= needed => at,
                    _ => joined.find(piece, needed)?,
                };
                *last = Some(at);
                Some(at.saturating_add(piece.len()))
            });
        }
        // Later starts search a suffix of this one's, so none of them can complete either.
        let Some(end) = end else {
            break;
        };
        let (first_line, last_line) =
            (joined.line_at(start), joined.line_at(end.saturating_sub(1)));
        if last_line.saturating_sub(first_line) < MAX_SPAN_LINES
            && lines.last() != Some(&first_line)
        {
            lines.push(first_line);
        }
        from = start.saturating_add(first.chars().next().map_or(1, char::len_utf8));
    }
    lines
}

/// The match nearest `cited`; of two equally near, the earlier line. `None` if there is none.
pub(super) fn nearest(matches: &[u64], cited: u64) -> Option<u64> {
    matches
        .iter()
        .copied()
        .min_by_key(|&line| (line.abs_diff(cited), line))
}

/// A file's lines, each normalised, joined with single spaces; blank ones are left out.
struct Joined {
    /// The joined text.
    text: String,
    /// Where each kept line starts in `text`, ascending, with its 1-based line number.
    starts: Vec<(usize, u64)>,
}

impl Joined {
    fn of(text: &str) -> Self {
        let mut joined = String::with_capacity(text.len());
        let mut starts = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = normalise(line);
            if line.is_empty() {
                continue;
            }
            if !joined.is_empty() {
                joined.push(' ');
            }
            starts.push((joined.len(), line_number(index)));
            joined.push_str(&line);
        }
        Self {
            text: joined,
            starts,
        }
    }

    /// The first byte offset at or after `from` where `piece` occurs.
    fn find(&self, piece: &str, from: usize) -> Option<usize> {
        let at = self.text.get(from..)?.find(piece)?;
        Some(from.saturating_add(at))
    }

    /// The line number of the line the byte at `offset` belongs to.
    fn line_at(&self, offset: usize) -> u64 {
        let index = self.starts.partition_point(|&(start, _)| start <= offset);
        index
            .checked_sub(1)
            .and_then(|index| self.starts.get(index))
            .map_or(1, |&(_, line)| line)
    }
}

/// The 1-based number of the line at `index`.
pub(super) fn line_number(index: usize) -> u64 {
    // `usize` is at most 64 bits on every target Rust supports, so this never saturates.
    u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{find, nearest};

    const FILE: &str = "\
fn a() {
    let x = y.unwrap();
}

fn b() {
    let x = y.unwrap(); // again
    foo(
        bar,
    );
}
";

    #[test]
    fn finds_every_line_a_span_starts_on_ignoring_trailing_comments() {
        assert_eq!(find(FILE, "let x = y.unwrap();"), [2, 6]);
    }

    #[test]
    fn matches_a_substring_of_a_line() {
        assert_eq!(find(FILE, "y.unwrap()"), [2, 6]);
    }

    #[test]
    fn matches_a_span_quoted_across_lines_on_its_first_line() {
        assert_eq!(find(FILE, "foo(\n  bar,\n);"), [7]);
    }

    #[test]
    fn matches_across_an_elision() {
        assert_eq!(find(FILE, "fn b() { ... foo("), [5]);
    }

    #[test]
    fn matches_every_start_that_shares_a_later_piece() {
        let text = "x();\nx();\nz();\ny();\n";

        assert_eq!(find(text, "x(); ... y();"), [1, 2]);
    }

    #[test]
    fn finds_nothing_for_absent_or_content_less_evidence() {
        assert_eq!(
            (find(FILE, "let z = 1;"), find(FILE, " ... \n…")),
            (vec![], vec![])
        );
    }

    #[test]
    fn refuses_a_match_spanning_more_lines_than_the_cap() {
        let mut text = String::from("start\n");
        text.push_str(&"filler\n".repeat(60));
        text.push_str("end\n");

        assert_eq!(find(&text, "start ... end"), Vec::<u64>::new());
    }

    #[test]
    fn keeps_a_cited_line_that_matches() {
        assert_eq!(nearest(&[2, 6], 6), Some(6));
    }

    #[test]
    fn moves_to_the_nearest_match() {
        assert_eq!(nearest(&[2, 6, 30], 9), Some(6));
    }

    #[test]
    fn breaks_a_tie_towards_the_earlier_line() {
        assert_eq!(nearest(&[2, 6], 4), Some(2));
    }

    proptest! {
        #[test]
        fn relocation_lands_on_a_matching_line_nearest_the_cited_one(
            lines in prop::collection::vec(prop::sample::select(vec!["a();", "b();", "", "c(a);"]), 0..40),
            cited in 1_u64..50,
        ) {
            let text = lines.join("\n");
            let matches = find(&text, "a();");

            let relocated = nearest(&matches, cited);

            match relocated {
                None => prop_assert!(matches.is_empty(), "a match exists"),
                Some(line) => {
                    let index = usize::try_from(line - 1).expect("lines fit usize");
                    prop_assert!(
                        text.lines().nth(index).is_some_and(|l| l.contains("a();")),
                        "line {line} does not hold the evidence"
                    );
                    prop_assert!(
                        matches.iter().all(|m| m.abs_diff(cited) >= line.abs_diff(cited)),
                        "a nearer match exists"
                    );
                }
            }
        }
    }
}
