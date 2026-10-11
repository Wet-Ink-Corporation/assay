//! `git diff --numstat -z --text`: per-file line counts for tiering.

use super::Malformed;

/// Lines added or deleted in one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCount {
    /// git counted this many lines.
    Counted(u64),
    /// git printed `-` even with `--text`: the file could not be measured, so it counts as
    /// maximum size (`docs/architecture/03-contracts.md` §9).
    Unmeasured,
}

impl LineCount {
    /// The count used for tiering: [`LineCount::Unmeasured`] is `u64::MAX`, which forces the
    /// largest tier.
    ///
    /// Sum these with `saturating_add` (or `checked_add` and treat overflow as maximum size):
    /// a plain `+` overflows on the first unmeasured file, and wrapping would turn "maximum
    /// size" into a small number.
    #[must_use]
    pub const fn for_tiering(self) -> u64 {
        match self {
            Self::Counted(lines) => lines,
            Self::Unmeasured => u64::MAX,
        }
    }
}

/// One changed file as `git diff --numstat` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    path: String,
    previous_path: Option<String>,
    added: LineCount,
    deleted: LineCount,
}

impl FileChange {
    /// The path at the head side of the diff.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The path at the base side, when git detected a rename or copy.
    #[must_use]
    pub fn previous_path(&self) -> Option<&str> {
        self.previous_path.as_deref()
    }

    /// Lines added.
    #[must_use]
    pub const fn added(&self) -> LineCount {
        self.added
    }

    /// Lines deleted.
    #[must_use]
    pub const fn deleted(&self) -> LineCount {
        self.deleted
    }
}

/// The result of measuring a diff under a wall-clock cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOutcome {
    /// git finished within the cap.
    Measured(Vec<FileChange>),
    /// git did not finish within the cap (rename detection is quadratic), so the PR is
    /// `needs-split` rather than left hanging (`docs/architecture/03-contracts.md` §9).
    NeedsSplit,
}

/// Parses `git diff --numstat -z` output. Non-UTF-8 paths are rejected, which fails closed.
pub(super) fn parse(stdout: &[u8]) -> Result<Vec<FileChange>, Malformed> {
    let mut changes = Vec::new();
    let Some(mut fields) = super::nul_terminated_fields(stdout)? else {
        return Ok(changes);
    };
    while let Some(record) = fields.next() {
        let mut columns = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (columns.next(), columns.next(), columns.next())
        else {
            return Err(Malformed("a numstat record has fewer than three columns"));
        };
        let (previous_path, path) = if path.is_empty() {
            let (Some(from), Some(to)) = (fields.next(), fields.next()) else {
                return Err(Malformed("a rename record is missing a path"));
            };
            (Some(from.to_owned()), to.to_owned())
        } else {
            (None, path.to_owned())
        };
        changes.push(FileChange {
            path,
            previous_path,
            added: count(added)?,
            deleted: count(deleted)?,
        });
    }
    Ok(changes)
}

/// One numstat count column: digits, or `-` for a file git did not measure.
fn count(column: &str) -> Result<LineCount, Malformed> {
    if column == "-" {
        return Ok(LineCount::Unmeasured);
    }
    column
        .parse()
        .map(LineCount::Counted)
        .map_err(|_not_a_number| Malformed("a numstat count is neither digits nor `-`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_counts_and_paths() {
        let changes = parse(b"3\t1\tsrc/lib.rs\x0010\t0\tCargo.toml\x00").expect("well formed");

        assert_eq!(
            changes,
            [
                FileChange {
                    path: "src/lib.rs".to_owned(),
                    previous_path: None,
                    added: LineCount::Counted(3),
                    deleted: LineCount::Counted(1),
                },
                FileChange {
                    path: "Cargo.toml".to_owned(),
                    previous_path: None,
                    added: LineCount::Counted(10),
                    deleted: LineCount::Counted(0),
                },
            ],
            "each record is added, deleted, path"
        );
    }

    #[test]
    fn a_residual_dash_is_unmeasured_and_counts_as_maximum_size() {
        let changes = parse(b"-\t-\tlogo.png\x00").expect("well formed");

        let [change] = changes.as_slice() else {
            panic!("one record expected, got {changes:?}");
        };
        assert_eq!(change.added(), LineCount::Unmeasured, "`-` is not a count");
        assert_eq!(
            change.added().for_tiering(),
            u64::MAX,
            "an unmeasured file must force the largest tier"
        );
    }

    #[test]
    fn reads_renames_as_old_and_new_path() {
        let changes = parse(b"2\t2\t\x00src/old.rs\x00src/new.rs\x00").expect("well formed");

        assert_eq!(
            changes,
            [FileChange {
                path: "src/new.rs".to_owned(),
                previous_path: Some("src/old.rs".to_owned()),
                added: LineCount::Counted(2),
                deleted: LineCount::Counted(2),
            }],
            "a rename record is followed by the old and the new path"
        );
    }

    #[test]
    fn reads_an_empty_diff_as_no_changes() {
        assert_eq!(parse(b""), Ok(Vec::new()), "no output means no changes");
    }

    #[test]
    fn rejects_truncated_and_garbled_records() {
        for (garbled, detail) in [
            (
                b"3\t1\x00".as_slice(),
                "a numstat record has fewer than three columns",
            ),
            (
                b"x\t1\tsrc/lib.rs\x00",
                "a numstat count is neither digits nor `-`",
            ),
            (
                b"2\t2\t\x00src/old.rs\x00",
                "a rename record is missing a path",
            ),
            (b"1\t1\tsrc/\xff.rs\x00", "a path is not UTF-8"),
            (b"1\t1\tsrc/lib.rs", "the output does not end with NUL"),
        ] {
            assert_eq!(
                parse(garbled),
                Err(Malformed(detail)),
                "{garbled:?} must not be read as a change"
            );
        }
    }
}
