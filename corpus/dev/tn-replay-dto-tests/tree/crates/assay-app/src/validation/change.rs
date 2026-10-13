//! The change under review as validation reads it: file contents at both ends and the diff
//! hunks, with renames resolved by `git diff -M` (04 §4 steps 2 and 3).

use std::collections::BTreeMap;

use assay_domain::Side;

/// The change under review, as data the caller read from git.
///
/// `head_files` and `base_files` hold at least every file a finding may cite; a file missing
/// from them has no evidence to find.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// File contents at head, by path.
    pub head_files: BTreeMap<String, String>,
    /// File contents at base, by path: what `side: LEFT` and deleted code are searched in.
    pub base_files: BTreeMap<String, String>,
    /// One entry per changed file, with renames detected (`git diff -M`).
    pub diffs: Vec<FileDiff>,
}

/// One changed file: its paths and its hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The file's paths at base and head.
    pub paths: DiffPaths,
    /// The hunks of its unified diff, as `@@ -base +head @@` headers give them.
    pub hunks: Vec<Hunk>,
}

/// How a changed file's path differs between base and head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffPaths {
    /// The file exists only at head.
    Added {
        /// Its path at head.
        head: String,
    },
    /// The file exists only at base.
    Deleted {
        /// Its path at base.
        base: String,
    },
    /// The file keeps its path.
    Modified {
        /// Its path at both ends.
        path: String,
    },
    /// The file moved (`git diff -M`).
    Renamed {
        /// Its path at base.
        base: String,
        /// Its path at head.
        head: String,
    },
}

impl DiffPaths {
    /// The file's path at head, if it has one.
    fn head(&self) -> Option<&str> {
        match self {
            Self::Added { head } | Self::Renamed { head, .. } => Some(head),
            Self::Modified { path } => Some(path),
            Self::Deleted { .. } => None,
        }
    }

    /// The file's path at base, if it has one.
    fn base(&self) -> Option<&str> {
        match self {
            Self::Deleted { base } | Self::Renamed { base, .. } => Some(base),
            Self::Modified { path } => Some(path),
            Self::Added { .. } => None,
        }
    }
}

/// One hunk of a unified diff: the lines it covers on each side, context included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    /// The base lines (`-start,count`).
    pub base: LineSpan,
    /// The head lines (`+start,count`).
    pub head: LineSpan,
}

/// A run of `count` lines from line `start` (1-based); empty when `count` is 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    /// The first line.
    pub start: u64,
    /// How many lines.
    pub count: u64,
}

impl LineSpan {
    /// Whether `line` is one of these lines.
    pub(super) fn contains(self, line: u64) -> bool {
        line.checked_sub(self.start)
            .is_some_and(|offset| offset < self.count)
    }
}

/// The text a cited file's line numbers refer to, and where in the change it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Text<'c> {
    /// The side it was read from.
    pub(super) side: Side,
    /// Its key in that side's files.
    pub(super) path: &'c str,
    /// Its contents.
    pub(super) content: &'c str,
}

/// A cited file, located in the change.
#[derive(Debug)]
pub(super) struct Located<'c> {
    /// Its path at head after rename resolution; its base path if it was deleted.
    pub(super) file_at_head: String,
    /// The text its line numbers refer to, if the caller supplied it.
    pub(super) text: Option<Text<'c>>,
    /// The side its line numbers refer to: base for `LEFT` and for a deleted file.
    pub(super) side: Side,
    /// Its diff, if it changed.
    diff: Option<&'c FileDiff>,
}

impl Located<'_> {
    /// The hunk span holding `line` on the file's side, if any.
    pub(super) fn hunk_of(&self, line: u64) -> Option<LineSpan> {
        self.diff?
            .hunks
            .iter()
            .map(|hunk| match self.side {
                Side::Right => hunk.head,
                Side::Left => hunk.base,
            })
            .find(|span| span.contains(line))
    }
}

impl Change {
    /// Locates `file`, cited on `side`: a head path or, for a renamed or deleted file, its
    /// base path. A file the diff does not list is unchanged, with one path at both ends.
    pub(super) fn locate(&self, file: &str, side: Side) -> Located<'_> {
        let diff = self
            .diffs
            .iter()
            .find(|diff| diff.paths.head() == Some(file))
            .or_else(|| {
                self.diffs
                    .iter()
                    .find(|diff| diff.paths.base() == Some(file))
            });
        let (head, base) = diff.map_or((Some(file), Some(file)), |diff| {
            (diff.paths.head(), diff.paths.base())
        });
        let side = if head.is_none() { Side::Left } else { side };
        let entry = match side {
            Side::Right => head.and_then(|path| self.head_files.get_key_value(path)),
            Side::Left => base.and_then(|path| self.base_files.get_key_value(path)),
        };
        Located {
            file_at_head: head.or(base).unwrap_or(file).to_owned(),
            text: entry.map(|(path, content)| Text {
                side,
                path,
                content,
            }),
            side,
            diff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `line` is in one of `located`'s hunks.
    fn in_diff(located: &Located<'_>, line: u64) -> bool {
        located.hunk_of(line).is_some()
    }

    fn renamed() -> Change {
        Change {
            head_files: BTreeMap::from([("new.rs".to_owned(), "head".to_owned())]),
            base_files: BTreeMap::from([("old.rs".to_owned(), "base".to_owned())]),
            diffs: vec![FileDiff {
                paths: DiffPaths::Renamed {
                    base: "old.rs".to_owned(),
                    head: "new.rs".to_owned(),
                },
                hunks: vec![Hunk {
                    base: LineSpan { start: 4, count: 2 },
                    head: LineSpan {
                        start: 10,
                        count: 3,
                    },
                }],
            }],
        }
    }

    #[test]
    fn a_span_holds_count_lines_from_start() {
        let span = LineSpan { start: 4, count: 2 };

        let held = [3, 4, 5, 6].map(|line| span.contains(line));

        assert_eq!(held, [false, true, true, false]);
    }

    #[test]
    fn an_empty_span_holds_nothing() {
        assert!(!LineSpan { start: 4, count: 0 }.contains(4));
    }

    #[test]
    fn resolves_the_base_path_of_a_rename_to_its_head_path_and_text() {
        let change = renamed();

        let located = change.locate("old.rs", Side::Right);

        assert_eq!(
            (located.file_at_head.as_str(), located.text),
            (
                "new.rs",
                Some(Text {
                    side: Side::Right,
                    path: "new.rs",
                    content: "head"
                })
            )
        );
    }

    #[test]
    fn reads_the_left_side_of_a_rename_at_its_base_path() {
        let change = renamed();

        let located = change.locate("new.rs", Side::Left);

        assert_eq!(
            (located.file_at_head.as_str(), located.text),
            (
                "new.rs",
                Some(Text {
                    side: Side::Left,
                    path: "old.rs",
                    content: "base"
                })
            )
        );
    }

    #[test]
    fn checks_hunks_on_the_cited_side() {
        let change = renamed();

        let (right, left) = (
            change.locate("new.rs", Side::Right),
            change.locate("new.rs", Side::Left),
        );

        assert_eq!(
            [
                in_diff(&right, 11),
                in_diff(&right, 4),
                in_diff(&left, 4),
                in_diff(&left, 11)
            ],
            [true, false, true, false]
        );
    }

    #[test]
    fn reads_a_deleted_file_at_base_whatever_the_side() {
        let change = Change {
            base_files: BTreeMap::from([("gone.rs".to_owned(), "base".to_owned())]),
            diffs: vec![FileDiff {
                paths: DiffPaths::Deleted {
                    base: "gone.rs".to_owned(),
                },
                hunks: vec![Hunk {
                    base: LineSpan { start: 1, count: 1 },
                    head: LineSpan { start: 0, count: 0 },
                }],
            }],
            ..Change::default()
        };

        let located = change.locate("gone.rs", Side::Right);

        assert_eq!(
            (
                located.file_at_head.as_str(),
                located.text.map(|text| (text.side, text.content)),
                in_diff(&located, 1)
            ),
            ("gone.rs", Some((Side::Left, "base")), true)
        );
    }

    #[test]
    fn reads_an_unchanged_file_at_the_cited_path_outside_any_diff() {
        let change = Change {
            head_files: BTreeMap::from([("same.rs".to_owned(), "text".to_owned())]),
            ..Change::default()
        };

        let located = change.locate("same.rs", Side::Right);

        assert_eq!(
            (located.text.map(|text| text.content), in_diff(&located, 1)),
            (Some("text"), false)
        );
    }

    #[test]
    fn finds_the_hunk_span_holding_a_line() {
        let change = renamed();

        let located = change.locate("new.rs", Side::Right);

        assert_eq!(
            (located.hunk_of(12), located.hunk_of(13)),
            (
                Some(LineSpan {
                    start: 10,
                    count: 3
                }),
                None
            )
        );
    }
}
