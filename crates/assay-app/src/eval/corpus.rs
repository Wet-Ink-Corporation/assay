//! One split of the eval corpus (`corpus/{dev,test,inject}`, 04 §8), as the caller listed it.
//!
//! A split holds an optional `README.md` and one directory per case, named by its id. A case
//! directory holds exactly `case.json`, `change.diff` and `tree/` (the reviewed head, which the
//! model reads; the labels stay outside it). Everything past a cap, anything else and anything
//! that is neither a directory nor a regular file is refused (wi-b5affa).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::{fmt, mem};

use assay_domain::ContentHash;
use thiserror::Error;

use super::case::{AuthoringModel, CaseProblem, CaseSource, Label, read_case};
use crate::check::MAX_DIFF_BYTES;
use crate::golden::{CorpusEntry, CorpusEntryKind};

/// The most entries a split may list, directories and tree files included.
pub const MAX_EVAL_ENTRIES: usize = 65_536;

/// The most cases a split may hold.
pub const MAX_EVAL_CASES: usize = 512;

/// The largest `case.json`, in bytes: 64 KiB.
pub const MAX_CASE_BYTES: usize = 64 << 10;

/// The longest case id, in bytes.
pub const MAX_CASE_ID_BYTES: usize = 64;

/// The case metadata and labels.
const CASE_FILE: &str = "case.json";
/// The change under review, as a unified diff.
const DIFF_FILE: &str = "change.diff";
/// The reviewed head the model may read.
const TREE_DIR: &str = "tree";
/// The one file allowed beside the cases.
const README: &str = "README.md";

/// A corpus split (04 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Split {
    /// Prompt work runs against it.
    Dev,
    /// Held out: never read during prompt iteration.
    Test,
    /// Hostile-text fixtures.
    Inject,
}

impl Split {
    /// The split's directory under `corpus/`.
    #[must_use]
    pub const fn dir(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Test => "test",
            Self::Inject => "inject",
        }
    }
}

impl fmt::Display for Split {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir())
    }
}

/// One case, read and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCase {
    id: String,
    authoring_model: AuthoringModel,
    source: CaseSource,
    labels: Vec<Label>,
    diff: String,
    content: ContentHash,
}

impl EvalCase {
    /// The case's id: its directory name.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Who wrote the change.
    #[must_use]
    pub const fn authoring_model(&self) -> &AuthoringModel {
        &self.authoring_model
    }

    /// Where the case came from.
    #[must_use]
    pub const fn source(&self) -> CaseSource {
        self.source
    }

    /// The fixing hunks; empty for a case with nothing to find.
    #[must_use]
    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    /// The change under review, as a unified diff.
    #[must_use]
    pub fn diff(&self) -> &str {
        &self.diff
    }

    /// The digest of the case's `case.json`, `change.diff` and every `tree/` entry (path and,
    /// for a file, its bytes), as read.
    #[must_use]
    pub const fn content(&self) -> ContentHash {
        self.content
    }
}

/// A split's cases, in id order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCorpus {
    split: Split,
    cases: Vec<EvalCase>,
}

impl EvalCorpus {
    /// The split.
    #[must_use]
    pub const fn split(&self) -> Split {
        self.split
    }

    /// The cases, in id order.
    #[must_use]
    pub fn cases(&self) -> &[EvalCase] {
        &self.cases
    }
}

/// Why a split was refused. Paths are relative to the split directory and shown escaped.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CorpusError {
    /// More than [`MAX_EVAL_ENTRIES`] entries were listed; none was looked at.
    #[error("the split lists more than {limit} entries")]
    TooManyEntries {
        /// The cap.
        limit: usize,
    },
    /// More than [`MAX_EVAL_CASES`] cases.
    #[error("the split holds more than {limit} cases")]
    TooManyCases {
        /// The cap.
        limit: usize,
    },
    /// An entry that does not belong (the first in path order): not a directory or regular
    /// file, a directory not named as a case id, or a file other than the case files.
    #[error("unexpected entry {path:?} in the eval corpus")]
    UnexpectedEntry {
        /// The entry.
        path: String,
    },
    /// A case lacks one of `case.json`, `change.diff` and `tree/`.
    #[error("case {case:?} has no {missing}")]
    Incomplete {
        /// The case id.
        case: String,
        /// The missing entry's name.
        missing: &'static str,
    },
    /// A case file is over its cap ([`MAX_CASE_BYTES`] for `case.json`, `MAX_DIFF_BYTES` for
    /// `change.diff`); it was not read.
    #[error("{path:?} is over {limit} bytes")]
    TooLarge {
        /// The file.
        path: String,
        /// The cap.
        limit: usize,
    },
    /// A case's `change.diff` is not UTF-8 text without NUL.
    #[error("the change.diff of case {case:?} is not text")]
    DiffNotText {
        /// The case id.
        case: String,
    },
    /// A case's `case.json` was refused.
    #[error("the case.json of case {case:?} was refused")]
    Case {
        /// The case id.
        case: String,
        /// Why.
        #[source]
        problem: CaseProblem,
    },
}

/// The entries of one case directory.
#[derive(Debug, Default)]
struct CaseEntries<'a> {
    json: Option<&'a [u8]>,
    diff: Option<&'a [u8]>,
    tree: bool,
    /// Every entry under `tree/`, by its path below the case directory: a file's bytes, or
    /// `None` for a directory. Ordered by path, as the digest folds them.
    tree_entries: BTreeMap<&'a str, Option<&'a [u8]>>,
}

/// Reads one split from every entry under its directory (at any depth; tree files' bytes are
/// not read). Linear in the entries after the [`MAX_EVAL_ENTRIES`] check, plus a sort of the
/// case ids.
///
/// # Examples
///
/// ```
/// use assay_app::{CorpusEntry, CorpusEntryKind, CorpusError, Split, read_eval_corpus};
///
/// let entries = [CorpusEntry {
///     path: "README.md".to_owned(),
///     kind: CorpusEntryKind::File(b"# dev\n".to_vec()),
/// }];
///
/// let corpus = read_eval_corpus(Split::Dev, &entries)?;
///
/// // A split with no case reads, but `run_eval` refuses to run it.
/// assert!(corpus.cases().is_empty());
/// # Ok::<(), CorpusError>(())
/// ```
///
/// # Errors
/// [`CorpusError`] for the first problem found: the entry cap, then unexpected entries (the
/// first in path order) and the case cap, then each case in id order.
pub fn read_eval_corpus(split: Split, entries: &[CorpusEntry]) -> Result<EvalCorpus, CorpusError> {
    if entries.len() > MAX_EVAL_ENTRIES {
        return Err(CorpusError::TooManyEntries {
            limit: MAX_EVAL_ENTRIES,
        });
    }
    let mut cases: BTreeMap<&str, CaseEntries<'_>> = BTreeMap::new();
    let mut unexpected: Option<&str> = None;
    for entry in entries {
        if !sort_entry(entry, &mut cases) {
            let path = entry.path.as_str();
            unexpected = Some(unexpected.map_or(path, |first| first.min(path)));
        }
    }
    if let Some(path) = unexpected {
        return Err(CorpusError::UnexpectedEntry {
            path: path.to_owned(),
        });
    }
    if cases.len() > MAX_EVAL_CASES {
        return Err(CorpusError::TooManyCases {
            limit: MAX_EVAL_CASES,
        });
    }
    let cases = cases
        .into_iter()
        .map(|(id, entries)| read_one(id, &entries))
        .collect::<Result<_, _>>()?;
    Ok(EvalCorpus { split, cases })
}

/// Files `entry` under its case; `false` when it does not belong in a split or repeats a case
/// file. Linear in the path's length.
fn sort_entry<'a>(entry: &'a CorpusEntry, cases: &mut BTreeMap<&'a str, CaseEntries<'a>>) -> bool {
    let (id, rest) = match entry.path.split_once('/') {
        Some((id, rest)) => (id, Some(rest)),
        None => (entry.path.as_str(), None),
    };
    let plain = |path: &str| {
        path.split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
    };
    if !rest.is_none_or(plain) {
        return false;
    }
    let file = match &entry.kind {
        CorpusEntryKind::File(bytes) => Some(bytes.as_slice()),
        CorpusEntryKind::Directory => None,
        CorpusEntryKind::Other => return false,
    };
    if id == README && rest.is_none() {
        return file.is_some();
    }
    if !is_case_id(id) {
        return false;
    }
    let case = cases.entry(id).or_default();
    // A repeated case file or tree is refused, never kept in place of the first.
    match (rest, file) {
        (None, None) => true,
        (Some(TREE_DIR), None) => !mem::replace(&mut case.tree, true),
        (Some(path), _) if path.starts_with("tree/") => {
            case.tree_entries.insert(path, file).is_none()
        }
        (Some(CASE_FILE), Some(bytes)) => case.json.replace(bytes).is_none(),
        (Some(DIFF_FILE), Some(bytes)) => case.diff.replace(bytes).is_none(),
        _ => false,
    }
}

/// Whether `id` names a case: 1 to [`MAX_CASE_ID_BYTES`] bytes of `a-z`, `0-9` and `-`.
fn is_case_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CASE_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The digest of a case: `case.json`, `change.diff`, then every tree entry in path order. A
/// directory is named with a trailing `/` and no content, which no file path can carry, so a
/// directory never hashes as an empty file.
fn digest(json: &[u8], diff: &[u8], tree: &BTreeMap<&str, Option<&[u8]>>) -> ContentHash {
    // Only a directory's name is built; a file's path is borrowed.
    let names: Vec<(Cow<'_, str>, &[u8])> = tree
        .iter()
        .map(|(path, file)| match file {
            Some(bytes) => (Cow::Borrowed(*path), *bytes),
            None => (Cow::Owned(format!("{path}/")), &[][..]),
        })
        .collect();
    let tree = names.iter().map(|(name, bytes)| (name.as_ref(), *bytes));
    ContentHash::of_files(
        [(CASE_FILE, json), (DIFF_FILE, diff)]
            .into_iter()
            .chain(tree),
    )
}

/// Checks and reads one case's files.
fn read_one(id: &str, entries: &CaseEntries<'_>) -> Result<EvalCase, CorpusError> {
    let incomplete = |missing| CorpusError::Incomplete {
        case: id.to_owned(),
        missing,
    };
    let json = entries.json.ok_or_else(|| incomplete(CASE_FILE))?;
    let diff = entries.diff.ok_or_else(|| incomplete(DIFF_FILE))?;
    if !entries.tree {
        return Err(incomplete(TREE_DIR));
    }
    let too_large = |file: &str, limit| CorpusError::TooLarge {
        path: format!("{id}/{file}"),
        limit,
    };
    if json.len() > MAX_CASE_BYTES {
        return Err(too_large(CASE_FILE, MAX_CASE_BYTES));
    }
    if diff.len() > MAX_DIFF_BYTES {
        return Err(too_large(DIFF_FILE, MAX_DIFF_BYTES));
    }
    let diff = str::from_utf8(diff)
        .ok()
        .filter(|text| !text.contains('\0'))
        .ok_or_else(|| CorpusError::DiffNotText {
            case: id.to_owned(),
        })?;
    let content = digest(json, diff.as_bytes(), &entries.tree_entries);
    let case = read_case(json).map_err(|problem| CorpusError::Case {
        case: id.to_owned(),
        problem,
    })?;
    Ok(EvalCase {
        id: id.to_owned(),
        authoring_model: case.authoring_model,
        source: case.source,
        labels: case.labels,
        diff: diff.to_owned(),
        content,
    })
}
