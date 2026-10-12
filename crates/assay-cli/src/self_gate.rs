//! `assay self-gate <corpus-dir> <rubric-file>`: the `assay-self` exec gate (02 §7, `05` T16).
//!
//! It reads the corpus and the rubric from the working directory (the head checkout) under
//! caps, hands them to [`assay_app::replay_golden`] and writes the gate's `assay.findings/1`
//! document to `ASSAY_FINDINGS_OUT` (03 §2). Anything it cannot read is an error, which the
//! binary reports as exit 2 (a gate error), never a pass.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use assay_app::{CorpusEntry, CorpusEntryKind, GoldenReplay, SELF_GATE, replay_golden};
use assay_domain::{GateConclusion, Source, SourceKind};

/// Most entries read under the corpus directory, directories included (H-13).
const MAX_ENTRIES: usize = 4_096;
/// Largest file read, in bytes: 1 MiB.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// Most bytes read from all files together: 64 MiB.
const MAX_TOTAL_BYTES: u64 = 64 << 20;

/// The variable naming the document's path, set by the exec adapter.
pub(crate) const FINDINGS_OUT: &str = "ASSAY_FINDINGS_OUT";
/// The variable carrying the run's key, passed on by the gate job.
pub(crate) const RUN_KEY: &str = "ASSAY_RUN_KEY";

/// Why the gate could not judge the corpus.
#[derive(Debug)]
pub(crate) enum SelfGateError {
    /// Not exactly two UTF-8 arguments.
    Usage,
    /// A variable the gate contract sets is missing or empty.
    Variable(&'static str),
    /// A variable the gate contract sets is not valid Unicode.
    VariableNotUnicode(&'static str),
    /// A path could not be listed or read.
    Read { path: PathBuf, source: io::Error },
    /// A path is not what it must be (the corpus a directory, the rubric a regular file) or
    /// has a name that is not UTF-8.
    Shape {
        path: PathBuf,
        expected: &'static str,
    },
    /// A read cap was passed.
    TooLarge { path: PathBuf },
    /// The document could not be encoded.
    Encode(serde_json::Error),
    /// The document could not be written.
    Write { path: PathBuf, source: io::Error },
}

impl fmt::Display for SelfGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str("usage: assay self-gate <corpus-dir> <rubric-file>"),
            Self::Variable(name) => write!(f, "{name} is not set"),
            Self::VariableNotUnicode(name) => write!(f, "{name} is not valid Unicode"),
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Shape { path, expected } => write!(f, "{} is not {expected}", path.display()),
            Self::TooLarge { path } => write!(f, "{} is past the read caps", path.display()),
            Self::Encode(source) => write!(f, "cannot encode the document: {source}"),
            Self::Write { path, source } => write!(f, "cannot write {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SelfGateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Encode(source) => Some(source),
            Self::Usage
            | Self::Variable(_)
            | Self::VariableNotUnicode(_)
            | Self::Shape { .. }
            | Self::TooLarge { .. } => None,
        }
    }
}

/// The gate contract's variables (03 §2), as the composition root read them.
#[derive(Debug)]
pub(crate) struct GateVariables {
    /// `ASSAY_FINDINGS_OUT`: where the document goes.
    pub(crate) findings_out: Option<OsString>,
    /// `ASSAY_RUN_KEY`: the run the document belongs to.
    pub(crate) run_key: Option<OsString>,
}

/// Runs the gate with `args` (after `self-gate`), writes its document and returns its
/// conclusion.
///
/// # Errors
///
/// [`SelfGateError`] when the arguments or variables are wrong, the corpus or rubric cannot be
/// read within the caps, or the document cannot be written; no document is then promised.
pub(crate) fn run(
    args: impl Iterator<Item = OsString>,
    variables: GateVariables,
) -> Result<GateConclusion, SelfGateError> {
    let [corpus_dir, rubric_path] = arguments(args)?;
    let out = variables
        .findings_out
        .filter(|value| !value.is_empty())
        .ok_or(SelfGateError::Variable(FINDINGS_OUT))?;
    let run_key = variables
        .run_key
        .filter(|value| !value.is_empty())
        .ok_or(SelfGateError::Variable(RUN_KEY))?
        .into_string()
        .map_err(|_not_unicode| SelfGateError::VariableNotUnicode(RUN_KEY))?;

    let mut budget = Budget::new();
    let entries = read_corpus(Path::new(&corpus_dir), &mut budget)?;
    let rubric = read_file(Path::new(&rubric_path), &mut budget)?;
    let source = Source {
        kind: SourceKind::Gate,
        name: SELF_GATE.to_owned(),
        version: concat!("assay/", env!("CARGO_PKG_VERSION")).to_owned(),
        role: None,
        run_key,
    };
    let document = replay_golden(
        &GoldenReplay {
            corpus_dir: corpus_dir.trim_end_matches('/'),
            entries: &entries,
            rubric_path: &rubric_path,
            rubric: &rubric,
        },
        source,
    );

    let bytes = serde_json::to_vec(&document).map_err(SelfGateError::Encode)?;
    let out = PathBuf::from(out);
    // `create_new` never writes through a path that already exists.
    File::create_new(&out)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(|source| SelfGateError::Write { path: out, source })?;
    Ok(document.conclusion.unwrap_or(GateConclusion::Fail))
}

/// Exactly two UTF-8 arguments.
fn arguments(mut args: impl Iterator<Item = OsString>) -> Result<[String; 2], SelfGateError> {
    let mut next = || args.next().and_then(|arg| arg.into_string().ok());
    match (next(), next(), next()) {
        (Some(corpus), Some(rubric), None) => Ok([corpus, rubric]),
        _ => Err(SelfGateError::Usage),
    }
}

/// What is left of the read caps.
#[derive(Debug)]
struct Budget {
    /// Entries still allowed under the corpus directory.
    entries: usize,
    /// Bytes still allowed across all files.
    bytes: u64,
    /// Largest single file, in bytes.
    file_bytes: u64,
}

impl Budget {
    /// The documented caps.
    const fn new() -> Self {
        Self::with(MAX_ENTRIES, MAX_TOTAL_BYTES, MAX_FILE_BYTES)
    }

    /// Caps of `entries` entries, `bytes` bytes in all and `file_bytes` per file.
    const fn with(entries: usize, bytes: u64, file_bytes: u64) -> Self {
        Self {
            entries,
            bytes,
            file_bytes,
        }
    }
}

/// Every entry under `dir`, depth first, paths relative to it; symlinks and other non-regular
/// files are listed as [`CorpusEntryKind::Other`], never followed or read.
fn read_corpus(dir: &Path, budget: &mut Budget) -> Result<Vec<CorpusEntry>, SelfGateError> {
    let is_dir = fs::symlink_metadata(dir)
        .map_err(|source| read_error(dir, source))?
        .is_dir();
    if !is_dir {
        return Err(SelfGateError::Shape {
            path: dir.to_path_buf(),
            expected: "a directory",
        });
    }
    let mut entries = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = pending.pop() {
        for entry in fs::read_dir(&path).map_err(|source| read_error(&path, source))? {
            let entry = entry.map_err(|source| read_error(&path, source))?;
            budget.entries = budget
                .entries
                .checked_sub(1)
                .ok_or_else(|| SelfGateError::TooLarge { path: entry.path() })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| SelfGateError::Shape {
                    path: path.join(name),
                    expected: "a UTF-8 name",
                })?;
            let child = entry.path();
            let relative = format!("{prefix}{name}");
            let file_type = entry
                .file_type()
                .map_err(|source| read_error(&child, source))?;
            let kind = if file_type.is_dir() {
                pending.push((child, format!("{relative}/")));
                CorpusEntryKind::Directory
            } else if file_type.is_file() {
                CorpusEntryKind::File(read_bounded(&child, budget)?)
            } else {
                CorpusEntryKind::Other
            };
            entries.push(CorpusEntry {
                path: relative,
                kind,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// The regular file at `path`, read within the caps.
fn read_file(path: &Path, budget: &mut Budget) -> Result<Vec<u8>, SelfGateError> {
    let is_file = fs::symlink_metadata(path)
        .map_err(|source| read_error(path, source))?
        .is_file();
    if !is_file {
        return Err(SelfGateError::Shape {
            path: path.to_path_buf(),
            expected: "a regular file",
        });
    }
    read_bounded(path, budget)
}

/// The bytes of the file at `path`, refused past the per-file cap or the remaining budget.
fn read_bounded(path: &Path, budget: &mut Budget) -> Result<Vec<u8>, SelfGateError> {
    let cap = budget.file_bytes.min(budget.bytes);
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(cap.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|source| read_error(path, source))?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read > cap {
        return Err(SelfGateError::TooLarge {
            path: path.to_path_buf(),
        });
    }
    budget.bytes = budget.bytes.saturating_sub(read);
    Ok(bytes)
}

fn read_error(path: &Path, source: io::Error) -> SelfGateError {
    SelfGateError::Read {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests;
