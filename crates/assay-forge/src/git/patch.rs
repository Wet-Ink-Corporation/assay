//! One changed file's unified diff and its hunks (local mode, 02 §6): the diff text for the
//! review's context pack, and the `@@ -base +head @@` spans validation places findings in
//! (04 §4 step 3).
//!
//! The hunk reader refuses rather than models (wi-b5affa): it accepts only the header lines git
//! prints for these flags, counts every hunk's lines against its header, and refuses a patch of
//! more than one file, whose hunks it could not tell apart.

use super::command::GitOperation;
use super::repository::ObjectRepo;
use super::sha::CommitSha;
use super::{GitError, Malformed};

/// A run of `count` lines from line `start` (1-based; 0 with a count of 0 for an empty side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    /// The first line.
    pub start: u64,
    /// How many lines.
    pub count: u64,
}

/// One hunk header: the lines it covers on each side, context included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HunkSpan {
    /// `-start,count`.
    pub base: LineRange,
    /// `+start,count`.
    pub head: LineRange,
}

/// One file's patch: git's unified diff and its hunks in order. Empty when the paths did not
/// change; a binary file has text but no hunks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilePatch {
    /// The unified diff as git printed it, at most the repository's output cap.
    pub text: Vec<u8>,
    /// Its hunk headers.
    pub hunks: Vec<HunkSpan>,
}

/// Header lines git prints before a file's hunks with `-M` and no `--binary`.
const HEADER_PREFIXES: [&str; 15] = [
    "diff --git ",
    "index ",
    "--- ",
    "+++ ",
    "new file mode ",
    "deleted file mode ",
    "old mode ",
    "new mode ",
    "similarity index ",
    "dissimilarity index ",
    "rename from ",
    "rename to ",
    "copy from ",
    "copy to ",
    "Binary files ",
];

impl ObjectRepo {
    /// The diff of `paths` (one file, or both sides of one rename) over `base...head`, with
    /// rename detection and three lines of context around each change, as GitHub shows a PR's
    /// hunks, and its hunk headers.
    ///
    /// # Errors
    /// [`GitError`] when the diff cannot run or fails, and [`GitError::Malformed`] when its
    /// output holds more than one file, a line this reader does not know, or a hunk whose lines
    /// do not match its header.
    pub fn file_patch(
        &self,
        base: &CommitSha,
        head: &CommitSha,
        paths: &[&str],
    ) -> Result<FilePatch, GitError> {
        let range = format!("{base}...{head}");
        let mut args = vec!["-M", "--unified=3", range.as_str(), "--"];
        args.extend_from_slice(paths);
        let captured = self.run(self.call(GitOperation::Diff, &args))?;
        let hunks = hunks(&captured.stdout).map_err(|Malformed(detail)| GitError::Malformed {
            operation: GitOperation::Diff,
            detail,
        })?;
        Ok(FilePatch {
            text: captured.stdout,
            hunks,
        })
    }
}

/// The hunk headers of a one-file patch, each checked against the lines that follow it.
fn hunks(patch: &[u8]) -> Result<Vec<HunkSpan>, Malformed> {
    let mut found = Vec::new();
    let mut files = 0_u32;
    // Lines the current hunk still owes on each side.
    let mut owed: Option<(u64, u64)> = None;
    let Some(body) = patch.strip_suffix(b"\n") else {
        return if patch.is_empty() {
            Ok(found)
        } else {
            Err(Malformed("the patch does not end with a newline"))
        };
    };
    for line in body.split(|&byte| byte == b'\n') {
        if let Some((base, head)) = owed {
            owed = consume(line, base, head)?;
            continue;
        }
        if line.starts_with(b"@@ ") {
            if files == 0 {
                return Err(Malformed("a hunk comes before its file header"));
            }
            let span = header(line)?;
            owed = owing(span);
            found.push(span);
        } else if line.starts_with(b"diff --git ") {
            files = files.saturating_add(1);
            if files > 1 {
                return Err(Malformed("the patch holds more than one file"));
            }
        } else if !is_header(line) {
            return Err(Malformed("a line is neither a header nor in a hunk"));
        }
    }
    if owed.is_some() {
        return Err(Malformed("the last hunk is shorter than its header"));
    }
    Ok(found)
}

/// The lines a hunk owes on each side, `None` when it owes none.
const fn owing(span: HunkSpan) -> Option<(u64, u64)> {
    if span.base.count == 0 && span.head.count == 0 {
        None
    } else {
        Some((span.base.count, span.head.count))
    }
}

/// What the hunk owes after `line`; `None` once it is complete.
fn consume(line: &[u8], base: u64, head: u64) -> Result<Option<(u64, u64)>, Malformed> {
    let short = Malformed("a hunk is longer than its header");
    let (base, head) = match line.first() {
        Some(b' ') => (
            base.checked_sub(1).ok_or(short)?,
            head.checked_sub(1).ok_or(short)?,
        ),
        Some(b'-') => (base.checked_sub(1).ok_or(short)?, head),
        Some(b'+') => (base, head.checked_sub(1).ok_or(short)?),
        // "\ No newline at end of file" belongs to the line before it.
        Some(b'\\') => (base, head),
        _ => return Err(Malformed("a hunk line has no diff marker")),
    };
    Ok((base > 0 || head > 0).then_some((base, head)))
}

/// Whether `line` is a file header line git prints for these flags.
fn is_header(line: &[u8]) -> bool {
    HEADER_PREFIXES
        .iter()
        .any(|prefix| line.starts_with(prefix.as_bytes()))
}

/// `@@ -a[,b] +c[,d] @@` with an optional section heading after it.
fn header(line: &[u8]) -> Result<HunkSpan, Malformed> {
    let bad = Malformed("a hunk header is not `@@ -a,b +c,d @@`");
    let text = str::from_utf8(line).map_err(|_utf8| bad)?;
    let rest = text.strip_prefix("@@ -").ok_or(bad)?;
    let (ranges, _heading) = rest.split_once(" @@").ok_or(bad)?;
    let (base, head) = ranges.split_once(" +").ok_or(bad)?;
    Ok(HunkSpan {
        base: range(base).ok_or(bad)?,
        head: range(head).ok_or(bad)?,
    })
}

/// `start[,count]`, the count 1 when absent, as git abbreviates it.
fn range(text: &str) -> Option<LineRange> {
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    Some(LineRange {
        start: number(start)?,
        count: number(count)?,
    })
}

/// ASCII digits only: no sign, no space.
fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn span(base: (u64, u64), head: (u64, u64)) -> HunkSpan {
        HunkSpan {
            base: LineRange {
                start: base.0,
                count: base.1,
            },
            head: LineRange {
                start: head.0,
                count: head.1,
            },
        }
    }

    const HEAD: &str = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n";

    fn read(hunks_text: &str) -> Result<Vec<HunkSpan>, Malformed> {
        hunks(format!("{HEAD}{hunks_text}").as_bytes())
    }

    #[test]
    fn reads_two_hunks_with_abbreviated_counts_and_headings() {
        let read = read("@@ -1 +1,2 @@ fn main\n-a\n+b\n+c\n@@ -9,2 +10,2 @@\n x\n-y\n+z\n");

        assert_eq!(read, Ok(vec![span((1, 1), (1, 2)), span((9, 2), (10, 2))]));
    }

    #[test]
    fn a_missing_newline_marker_counts_no_line() {
        let read = read("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n");

        assert_eq!(read, Ok(vec![span((1, 1), (1, 1))]));
    }

    #[test]
    fn a_hunk_longer_than_its_header_is_refused() {
        // The extra `+d` arrives after the hunk is complete, where only headers may follow.
        let read = read("@@ -1 +1 @@\n-a\n+b\n+d\n");

        assert_eq!(
            read,
            Err(Malformed("a line is neither a header nor in a hunk"))
        );
    }

    #[test]
    fn a_hunk_line_past_one_side_is_refused() {
        let read = read("@@ -1,2 +1 @@\n+a\n+b\n");

        assert_eq!(read, Err(Malformed("a hunk is longer than its header")));
    }

    #[test]
    fn a_hunk_shorter_than_its_header_is_refused() {
        let read = read("@@ -1,2 +1,2 @@\n a\n");

        assert_eq!(
            read,
            Err(Malformed("the last hunk is shorter than its header"))
        );
    }

    #[test]
    fn a_hunk_line_without_a_marker_is_refused() {
        let read = read("@@ -1 +1 @@\nplain\n");

        assert_eq!(read, Err(Malformed("a hunk line has no diff marker")));
    }

    #[test]
    fn a_malformed_header_is_refused() {
        for header in ["@@ -1 +x @@", "@@ -1 1 @@", "@@ --1 +1 @@", "@@ -1 +1"] {
            let read = read(&format!("{header}\n"));

            assert_eq!(
                read,
                Err(Malformed("a hunk header is not `@@ -a,b +c,d @@`")),
                "{header:?}"
            );
        }
    }

    #[test]
    fn a_hunk_before_any_file_is_refused() {
        assert_eq!(
            hunks(b"@@ -1 +1 @@\n-a\n+b\n"),
            Err(Malformed("a hunk comes before its file header"))
        );
    }

    #[test]
    fn an_unknown_header_line_is_refused() {
        assert_eq!(
            hunks(b"diff --git a/x b/x\nGIT binary patch\n"),
            Err(Malformed("a line is neither a header nor in a hunk"))
        );
    }

    #[test]
    fn output_without_a_final_newline_is_refused() {
        assert_eq!(
            hunks(b"diff --git a/x b/x"),
            Err(Malformed("the patch does not end with a newline"))
        );
    }

    #[test]
    fn empty_output_has_no_hunks() {
        assert_eq!(hunks(b""), Ok(Vec::new()));
    }
}
