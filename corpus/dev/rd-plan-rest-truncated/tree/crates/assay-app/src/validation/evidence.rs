//! Evidence location, 04 §4 step 2: where a normalised evidence span occurs in a whole file,
//! and which occurrence a drifted line moves to.

use assay_domain::normalise;

/// The normaliser's wildcard, which `...` and `…` both become (03 §3).
const WILDCARD: char = '\u{2026}';

/// The most lines one match may span, so a wildcard cannot stretch across a whole file.
const MAX_SPAN_LINES: u64 = 50;

/// The fewest non-whitespace characters normalised evidence must have, summed over the pieces
/// between wildcards, to locate anything reliably (Weigh-In default wi-b7959c).
const MIN_CONTENT_CHARS: usize = 12;

/// Where one occurrence of evidence lies: the line it starts on and the line it ends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Match {
    /// The 1-based line the match starts on.
    pub(super) line: u64,
    /// The 1-based line the match ends on, at least `line`.
    pub(super) last: u64,
}

/// Whether `evidence`, normalised, has at least [`MIN_CONTENT_CHARS`] non-whitespace
/// characters outside its wildcards.
pub(super) fn has_enough_content(evidence: &str) -> bool {
    normalise(evidence)
        .chars()
        .filter(|&c| c != WILDCARD && !c.is_whitespace())
        .count()
        >= MIN_CONTENT_CHARS
}

/// The match nearest `cited` by start line; of two equally near, the earlier. `None` if there
/// is none.
pub(super) fn nearest(matches: &[Match], cited: u64) -> Option<Match> {
    matches
        .iter()
        .copied()
        .min_by_key(|found| (found.line.abs_diff(cited), found.line))
}

/// A file's lines, each normalised, joined with single spaces; blank ones are left out. Built
/// once per file and validation run, then searched for every finding that cites the file.
#[derive(Debug)]
pub(super) struct Joined {
    /// The joined text.
    text: String,
    /// Where each kept line starts in `text`, ascending, with its 1-based line number.
    starts: Vec<(usize, u64)>,
}

impl Joined {
    /// Normalises and joins the lines of `text`.
    pub(super) fn of(text: &str) -> Self {
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

    /// [`Joined::find_normalised`] after normalising `evidence`.
    #[cfg(test)]
    pub(super) fn find(&self, evidence: &str) -> Vec<Match> {
        self.find_normalised(&normalise(evidence))
    }

    /// Every occurrence of evidence normalised with the shared normaliser (`normalised`), one
    /// per start line, ascending.
    ///
    /// Normalised, a span quoted across lines matches as it would in one string. Its pieces
    /// between wildcards must occur in order, the
    /// first starting on the match line, and the match must end within [`MAX_SPAN_LINES`] lines
    /// of it. Evidence without content matches nowhere.
    ///
    /// Cost: linear in the joined text plus, per kept line, the evidence's length. The first
    /// piece is searched from the start of each line after the last one tried, since a line
    /// yields one match at most and a later start on the same line can only end later; each
    /// later piece is matched at its first occurrence after the previous one, and as starts only
    /// move forward so do those occurrences, so each later piece is searched forward once.
    pub(super) fn find_normalised(&self, normalised: &str) -> Vec<Match> {
        self.search(normalised, &mut 0)
    }

    /// [`Joined::find_normalised`], adding to `work` the bytes each search covered.
    fn search(&self, normalised: &str, work: &mut usize) -> Vec<Match> {
        let pieces: Vec<&str> = normalised
            .split(WILDCARD)
            .filter(|piece| !piece.is_empty())
            .collect();
        let Some((first, rest)) = pieces.split_first() else {
            return Vec::new();
        };
        let mut matches: Vec<Match> = Vec::new();
        let mut found: Vec<Option<usize>> = vec![None; rest.len()];
        let mut from = 0;
        while let Some(start) = self.find_piece(first, from, work) {
            let mut end = Some(start.saturating_add(first.len()));
            for (piece, last) in rest.iter().zip(found.iter_mut()) {
                end = end.and_then(|needed| {
                    let at = match *last {
                        Some(at) if at >= needed => at,
                        _ => self.find_piece(piece, needed, work)?,
                    };
                    *last = Some(at);
                    Some(at.saturating_add(piece.len()))
                });
            }
            // Later starts search a suffix of this one's, so none of them can complete either.
            let Some(end) = end else {
                break;
            };
            let found = Match {
                line: self.line_at(start),
                last: self.line_at(end.saturating_sub(1)),
            };
            if found.last.saturating_sub(found.line) < MAX_SPAN_LINES {
                matches.push(found);
            }
            // Every later start on this line would give this line again, ending no earlier.
            let Some(next) = self.next_line_start(start) else {
                break;
            };
            from = next;
        }
        matches
    }

    /// The first byte offset at or after `from` where `piece` occurs, adding to `work` the bytes
    /// the search covered.
    fn find_piece(&self, piece: &str, from: usize, work: &mut usize) -> Option<usize> {
        let rest = self.text.get(from..)?;
        let found = rest.find(piece);
        let covered = found.map_or(rest.len(), |at| at.saturating_add(piece.len()));
        *work = work.saturating_add(covered);
        Some(from.saturating_add(found?))
    }

    /// Where the kept line after the one holding byte `offset` starts, if there is one.
    fn next_line_start(&self, offset: usize) -> Option<usize> {
        let index = self.starts.partition_point(|&(start, _)| start <= offset);
        self.starts.get(index).map(|&(start, _)| start)
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

    use super::{Joined, Match, has_enough_content, nearest, normalise};

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

    /// The lines `evidence` starts on in `text`.
    fn find(text: &str, evidence: &str) -> Vec<u64> {
        Joined::of(text)
            .find(evidence)
            .iter()
            .map(|found| found.line)
            .collect()
    }

    /// Single-line matches on `lines`.
    fn on(lines: &[u64]) -> Vec<Match> {
        lines
            .iter()
            .map(|&line| Match { line, last: line })
            .collect()
    }

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
    fn records_the_line_an_elided_match_ends_on() {
        assert_eq!(
            Joined::of(FILE).find("fn b() { ... foo("),
            [Match { line: 5, last: 7 }]
        );
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
    fn searches_a_long_repetitive_line_once_not_once_per_character() {
        let text = format!("{}\nlet tail = 1;\n", "a".repeat(1_000_000));
        let joined = Joined::of(&text);
        let mut work = 0;

        let matches = joined.search(&normalise(&"a".repeat(400)), &mut work);

        assert_eq!(matches, [Match { line: 1, last: 1 }]);
        assert!(
            work <= 2 * text.len(),
            "{work} bytes searched for {} bytes of text",
            text.len()
        );
    }

    #[test]
    fn searches_repetitive_lines_once_each() {
        let text = format!("{}\n", "ab".repeat(500)).repeat(1_000);
        let joined = Joined::of(&text);
        let mut work = 0;

        let matches = joined.search(
            &normalise(&format!("{} ... b", "ab".repeat(200))),
            &mut work,
        );

        assert_eq!(matches.len(), 1_000);
        assert!(
            work <= 3 * text.len(),
            "{work} bytes searched for {} bytes of text",
            text.len()
        );
    }

    #[test]
    fn counts_twelve_characters_of_content_across_pieces_and_not_whitespace() {
        let counted = [
            "let x = y.unwrap();",
            "abcdef ... ghijkl",
            "abcdef ... ghijk",
            "  a b c d e f g h i j k  ",
            "... // a comment that normalises away",
        ]
        .map(has_enough_content);

        assert_eq!(counted, [true, true, false, false, false]);
    }

    #[test]
    fn keeps_a_cited_line_that_matches() {
        assert_eq!(nearest(&on(&[2, 6]), 6).map(|m| m.line), Some(6));
    }

    #[test]
    fn moves_to_the_nearest_match() {
        assert_eq!(nearest(&on(&[2, 6, 30]), 9).map(|m| m.line), Some(6));
    }

    #[test]
    fn breaks_a_tie_towards_the_earlier_line() {
        assert_eq!(nearest(&on(&[2, 6]), 4).map(|m| m.line), Some(2));
    }

    proptest! {
        #[test]
        fn relocation_lands_on_a_matching_line_nearest_the_cited_one(
            lines in prop::collection::vec(
                prop::sample::select(vec!["a();", "b();", "", "c(a);"]),
                0..40,
            ),
            cited in 1_u64..50,
        ) {
            let text = lines.join("\n");
            let matches = Joined::of(&text).find("a();");

            let relocated = nearest(&matches, cited);

            match relocated {
                None => prop_assert!(matches.is_empty(), "a match exists"),
                Some(found) => {
                    let index = usize::try_from(found.line - 1).expect("lines fit usize");
                    prop_assert!(
                        text.lines().nth(index).is_some_and(|l| l.contains("a();")),
                        "line {} does not hold the evidence", found.line
                    );
                    prop_assert!(
                        matches
                            .iter()
                            .all(|m| m.line.abs_diff(cited) >= found.line.abs_diff(cited)),
                        "a nearer match exists"
                    );
                }
            }
        }
    }
}
