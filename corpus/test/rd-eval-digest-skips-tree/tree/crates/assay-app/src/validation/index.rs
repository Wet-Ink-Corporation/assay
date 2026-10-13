//! Per-file indexes built once per validation run and shared by every finding that cites the
//! file: its lines, its normalised search text and the function each line belongs to.

use std::collections::HashMap;
use std::rc::Rc;

use assay_domain::{Side, normalise};

use super::change::Text;
use super::evidence::{Joined, Match};
use super::symbol;

/// The most characters evidence may hold (`findings-1.schema.json`, `$defs.evidence`), so the
/// tree's lines that replace it stay within the contract.
const MAX_EVIDENCE_CHARS: usize = 400;

/// One file's text, indexed for evidence search.
#[derive(Debug)]
pub(super) struct Indexed<'c> {
    /// Its lines, without terminators.
    lines: Vec<&'c str>,
    /// Its normalised lines, for evidence search.
    joined: Joined,
    /// The function each line belongs to, by 0-based line index.
    functions: Vec<&'c str>,
    /// The matches of each evidence searched for so far, so many findings quoting the same
    /// text search the file once.
    found: HashMap<String, Rc<[Match]>>,
}

impl<'c> Indexed<'c> {
    /// Indexes `content`: one pass for the lines, one for the search text and one for the
    /// enclosing functions.
    fn of(content: &'c str) -> Self {
        Self {
            lines: content.lines().collect(),
            joined: Joined::of(content),
            functions: symbol::enclosing_functions(content),
            found: HashMap::new(),
        }
    }

    /// Every occurrence of `evidence`, one per start line, ascending; searched once per
    /// distinct `evidence` and shared after that.
    pub(super) fn find(&mut self, evidence: &str) -> Rc<[Match]> {
        // Keyed by the normalised evidence: quotes that normalise alike match alike.
        let normalised = normalise(evidence);
        if let Some(found) = self.found.get(&normalised) {
            return Rc::clone(found);
        }
        let found: Rc<[Match]> = self.joined.find_normalised(&normalised).into();
        self.found.insert(normalised, Rc::clone(&found));
        found
    }

    /// How many lines the file has.
    pub(super) fn line_count(&self) -> u64 {
        // `usize` is at most 64 bits on every target Rust supports, so this never saturates.
        u64::try_from(self.lines.len()).unwrap_or(u64::MAX)
    }

    /// The text of 1-based `line`, if the file has it.
    pub(super) fn line(&self, line: u64) -> Option<&'c str> {
        self.lines.get(index(line)?).copied()
    }

    /// The function 1-based `line` belongs to (empty at file scope), if the file has the line.
    pub(super) fn function_at(&self, line: u64) -> Option<&'c str> {
        self.functions.get(index(line)?).copied()
    }

    /// The file's own lines `found` spans, joined with `\n` and cut to the evidence cap: what
    /// replaces the evidence a reviewer quoted, so no model text survives in it. `None` if the
    /// file lacks those lines, which no match of this file's search text does.
    pub(super) fn span(&self, found: Match) -> Option<String> {
        let lines = self.lines.get(index(found.line)?..=index(found.last)?)?;
        Some(lines.join("\n").chars().take(MAX_EVIDENCE_CHARS).collect())
    }
}

/// The 0-based index of 1-based `line`.
fn index(line: u64) -> Option<usize> {
    usize::try_from(line.checked_sub(1)?).ok()
}

/// The files of one validation run, each indexed the first time a finding cites it.
#[derive(Debug, Default)]
pub(super) struct Files<'c> {
    indexed: HashMap<(Side, &'c str), Indexed<'c>>,
}

impl<'c> Files<'c> {
    /// The index of `text`, built on first use.
    pub(super) fn index(&mut self, text: Text<'c>) -> &mut Indexed<'c> {
        self.indexed
            .entry((text.side, text.path))
            .or_insert_with(|| Indexed::of(text.content))
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Files, Match, Text};
    use assay_domain::Side;

    const FILE: &str = "fn f() {\n    a();\n    b();\n}\n";

    #[test]
    fn replaces_a_match_with_the_lines_it_spans() {
        let mut files = Files::default();
        let text = Text {
            side: Side::Right,
            path: "f.rs",
            content: FILE,
        };

        let span = files.index(text).span(Match { line: 2, last: 3 });

        assert_eq!(span.as_deref(), Some("    a();\n    b();"));
    }

    #[test]
    fn answers_lines_and_functions_within_the_file_only() {
        let mut files = Files::default();
        let indexed = files.index(Text {
            side: Side::Right,
            path: "f.rs",
            content: FILE,
        });

        assert_eq!(
            (
                indexed.line_count(),
                indexed.line(2),
                indexed.function_at(3),
                indexed.line(5),
                indexed.line(0)
            ),
            (4, Some("    a();"), Some("f"), None, None)
        );
    }

    #[test]
    fn searches_a_file_once_per_distinct_evidence() {
        let mut files = Files::default();
        let indexed = files.index(Text {
            side: Side::Right,
            path: "f.rs",
            content: FILE,
        });

        // Both normalise to `a();`, so they are one search.
        let (first, again) = (indexed.find("a();"), indexed.find("a();   // again"));

        assert!(
            Rc::ptr_eq(&first, &again),
            "the second search reuses the first"
        );
        assert_eq!(*first, [Match { line: 2, last: 2 }]);
    }

    #[test]
    fn cuts_a_replacement_to_the_evidence_cap() {
        let long = format!("{}\n", "x".repeat(500));
        let mut files = Files::default();
        let indexed = files.index(Text {
            side: Side::Left,
            path: "long.rs",
            content: &long,
        });

        let span = indexed.span(Match { line: 1, last: 1 });

        assert_eq!(span.map(|span| span.chars().count()), Some(400));
    }
}
