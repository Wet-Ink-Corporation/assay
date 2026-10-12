//! The PR's file list for `plan` (`docs/architecture/04-pipeline.md` §2): `git diff --raw -z`
//! (the name-status with both modes) joined with `git diff --numstat -z --text`, both over
//! `base...head`.

use super::Malformed;
use super::numstat::{self, LineCount};
use super::tree::{self, EntryKind};

/// One changed file: its paths, what git records at each side and its line counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    path: String,
    previous_path: Option<String>,
    base_kind: Option<EntryKind>,
    head_kind: Option<EntryKind>,
    added: LineCount,
    deleted: LineCount,
}

impl ChangedFile {
    /// The path at the head side, or at the base side for a deletion.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The path at the base side, when git detected a rename or copy.
    #[must_use]
    pub fn previous_path(&self) -> Option<&str> {
        self.previous_path.as_deref()
    }

    /// What the base side records at the path; `None` for an addition.
    #[must_use]
    pub const fn base_kind(&self) -> Option<EntryKind> {
        self.base_kind
    }

    /// What the head side records at the path; `None` for a deletion.
    #[must_use]
    pub const fn head_kind(&self) -> Option<EntryKind> {
        self.head_kind
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

    /// The path and the previous path, without copying them.
    #[must_use]
    pub fn into_paths(self) -> (String, Option<String>) {
        (self.path, self.previous_path)
    }
}

/// The PR's changed files, or none when git could not list them within the cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeListing {
    /// git finished within the cap: every changed file, in git's order.
    Listed(Vec<ChangedFile>),
    /// git did not finish within the cap (rename detection is quadratic), so there is no list
    /// and the PR needs a split (`docs/architecture/03-contracts.md` §9).
    NeedsSplit,
}

/// One `--raw` record: the paths and what each side records.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawRecord {
    path: String,
    previous_path: Option<String>,
    base_kind: Option<EntryKind>,
    head_kind: Option<EntryKind>,
}

/// Joins `git diff --raw -z --no-abbrev` and `git diff --numstat -z` output of the same range
/// and rename detection, record by record. Records that disagree on their paths, or lists of
/// different lengths, are refused rather than matched up some other way.
pub(super) fn parse(raw: &[u8], numstat: &[u8]) -> Result<Vec<ChangedFile>, Malformed> {
    let records = parse_raw(raw)?;
    let counts = numstat::parse(numstat)?;
    if records.len() != counts.len() {
        return Err(Malformed("raw and numstat list different numbers of files"));
    }
    records
        .into_iter()
        .zip(counts)
        .map(|(record, count)| {
            if record.path != count.path()
                || record.previous_path.as_deref() != count.previous_path()
            {
                return Err(Malformed("raw and numstat disagree on a file's paths"));
            }
            Ok(ChangedFile {
                path: record.path,
                previous_path: record.previous_path,
                base_kind: record.base_kind,
                head_kind: record.head_kind,
                added: count.added(),
                deleted: count.deleted(),
            })
        })
        .collect()
}

/// Parses `--raw -z --no-abbrev` output: `:<mode> <mode> <id> <id> <status>`, then the path, or
/// for a rename or copy the old and the new path, each NUL-terminated.
fn parse_raw(stdout: &[u8]) -> Result<Vec<RawRecord>, Malformed> {
    let mut records = Vec::new();
    let Some(mut fields) = super::nul_terminated_fields(stdout)? else {
        return Ok(records);
    };
    while let Some(meta) = fields.next() {
        let (base_kind, head_kind, status) = parse_meta(meta)?;
        let first = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or(Malformed("a raw record has no path"))?;
        let (previous_path, path) = match status {
            Status::Paired => {
                let second = fields
                    .next()
                    .filter(|path| !path.is_empty())
                    .ok_or(Malformed("a rename record is missing a path"))?;
                (Some(first.to_owned()), second.to_owned())
            }
            Status::Single => (None, first.to_owned()),
        };
        records.push(RawRecord {
            path,
            previous_path,
            base_kind,
            head_kind,
        });
    }
    Ok(records)
}

/// Whether a record names one path or a pair (a rename or copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Single,
    Paired,
}

/// Both sides' kinds and the shape of one record, checked against each other: an addition has
/// no base side, a deletion no head side, and anything else both.
fn parse_meta(meta: &str) -> Result<(Option<EntryKind>, Option<EntryKind>, Status), Malformed> {
    let mut parts = meta
        .strip_prefix(':')
        .ok_or(Malformed("a raw record does not start with `:`"))?
        .split(' ');
    let (Some(base_mode), Some(head_mode), Some(base_id), Some(head_id), Some(status), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(Malformed(
            "a raw record is not <mode> <mode> <id> <id> <status>",
        ));
    };
    if !tree::is_object_id(base_id) || !tree::is_object_id(head_id) {
        return Err(Malformed("a raw record has an abbreviated object id"));
    }
    let base_kind = kind(base_mode)?;
    let head_kind = kind(head_mode)?;
    let unknown = Malformed("a raw record has an unknown or inconsistent status");
    let Some((letter, score)) = status.split_at_checked(1) else {
        return Err(unknown);
    };
    let shape = match (letter, base_kind.is_some(), head_kind.is_some()) {
        ("A", false, true) | ("D", true, false) | ("M" | "T", true, true) if score.is_empty() => {
            Status::Single
        }
        ("R" | "C", true, true) if is_score(score) => Status::Paired,
        _ => return Err(unknown),
    };
    Ok((base_kind, head_kind, shape))
}

/// A rename or copy score: one to three digits.
fn is_score(score: &str) -> bool {
    (1..=3).contains(&score.len()) && score.bytes().all(|byte| byte.is_ascii_digit())
}

/// What a raw mode records; `000000` is an absent side.
fn kind(mode: &str) -> Result<Option<EntryKind>, Malformed> {
    match mode {
        "000000" => Ok(None),
        "100644" => Ok(Some(EntryKind::File)),
        "100755" => Ok(Some(EntryKind::Executable)),
        "120000" => Ok(Some(EntryKind::Symlink)),
        "160000" => Ok(Some(EntryKind::Submodule)),
        _ => Err(Malformed("a raw record has an unknown mode")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn raw(records: &[(&str, &[&str])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (meta, paths) in records {
            out.extend_from_slice(meta.as_bytes());
            out.push(0);
            for path in *paths {
                out.extend_from_slice(path.as_bytes());
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn joins_a_modification_and_a_rename_with_their_counts() {
        let raw = raw(&[
            (&format!(":100644 100755 {A} {B} M"), &["run.sh"]),
            (
                &format!(":100644 100644 {A} {B} R087"),
                &["old.rs", "new.rs"],
            ),
        ]);
        let numstat = b"1\t0\trun.sh\x002\t3\t\x00old.rs\x00new.rs\x00";

        let files = parse(&raw, numstat);

        assert_eq!(
            files,
            Ok(vec![
                ChangedFile {
                    path: "run.sh".to_owned(),
                    previous_path: None,
                    base_kind: Some(EntryKind::File),
                    head_kind: Some(EntryKind::Executable),
                    added: LineCount::Counted(1),
                    deleted: LineCount::Counted(0),
                },
                ChangedFile {
                    path: "new.rs".to_owned(),
                    previous_path: Some("old.rs".to_owned()),
                    base_kind: Some(EntryKind::File),
                    head_kind: Some(EntryKind::File),
                    added: LineCount::Counted(2),
                    deleted: LineCount::Counted(3),
                },
            ])
        );
    }

    #[test]
    fn an_empty_diff_lists_nothing() {
        assert_eq!(parse(b"", b""), Ok(Vec::new()));
    }

    #[test]
    fn refuses_raw_and_numstat_that_disagree() {
        let one = raw(&[(&format!(":000000 100644 {ZERO} {B} A"), &["a.rs"])]);
        for (raw, numstat, detail) in [
            (
                one.as_slice(),
                b"1\t0\tb.rs\x00".as_slice(),
                "raw and numstat disagree on a file's paths",
            ),
            (
                one.as_slice(),
                b"".as_slice(),
                "raw and numstat list different numbers of files",
            ),
            (
                one.as_slice(),
                b"1\t0\ta.rs\x001\t0\tb.rs\x00".as_slice(),
                "raw and numstat list different numbers of files",
            ),
        ] {
            assert_eq!(parse(raw, numstat), Err(Malformed(detail)), "{numstat:?}");
        }
    }

    #[test]
    fn refuses_garbled_raw_records() {
        for (meta, paths, detail) in [
            (
                format!("100644 100644 {A} {B} M"),
                &["a.rs"][..],
                "a raw record does not start with `:`",
            ),
            (
                format!(":100644 100644 {A} M"),
                &["a.rs"][..],
                "a raw record is not <mode> <mode> <id> <id> <status>",
            ),
            (
                format!(":100644 100644 1111111 {B} M"),
                &["a.rs"][..],
                "a raw record has an abbreviated object id",
            ),
            (
                format!(":040000 100644 {A} {B} M"),
                &["a.rs"][..],
                "a raw record has an unknown mode",
            ),
            (
                format!(":100644 100644 {A} {B} U"),
                &["a.rs"][..],
                "a raw record has an unknown or inconsistent status",
            ),
            (
                format!(":100644 100644 {A} {B} A"),
                &["a.rs"][..],
                "a raw record has an unknown or inconsistent status",
            ),
            (
                format!(":000000 100644 {ZERO} {B} D"),
                &["a.rs"][..],
                "a raw record has an unknown or inconsistent status",
            ),
            (
                format!(":100644 100644 {A} {B} R"),
                &["a.rs", "b.rs"][..],
                "a raw record has an unknown or inconsistent status",
            ),
            (
                format!(":100644 100644 {A} {B} R100"),
                &["a.rs"][..],
                "a rename record is missing a path",
            ),
            (
                format!(":100644 100644 {A} {B} M"),
                &[""][..],
                "a raw record has no path",
            ),
        ] {
            let raw = raw(&[(&meta, paths)]);

            assert_eq!(parse_raw(&raw), Err(Malformed(detail)), "{meta}");
        }
    }
}
