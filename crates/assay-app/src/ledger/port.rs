//! The `Ledger` port: the `assay-ledger` repository's files, read whole and written by
//! compare-and-set on the file's version (06 §1). Implemented by the forge adapter (#70) with
//! the `assay-ledger-writer` App's token (ADR-26).

use std::fmt;
use std::num::NonZeroU64;

use super::time::LedgerMonth;

/// The longest ledger file read or written, in bytes (4 MiB): about 10 000 cost records, a
/// month at far above the expected load. A longer file is refused unparsed (H-13).
///
/// It is half of [`MAX_DOCUMENT_BYTES`](crate::MAX_DOCUMENT_BYTES) so that the file's base64
/// form, with GitHub's line breaks, fits in one API response under that cap.
pub const MAX_LEDGER_FILE_BYTES: usize = 4 << 20;

/// The longest line of a record stream, newline excluded, in bytes.
pub const MAX_RECORD_BYTES: usize = 4096;

/// How many times a write is attempted against a file that keeps changing under it before
/// [`LedgerWriteError::Contended`](super::LedgerWriteError::Contended).
pub const MAX_CAS_ATTEMPTS: u8 = 5;

/// The ledger repository's files.
///
/// Every byte read back is untrusted: anyone who can write the ledger repository could have
/// written it (T23), so the ledger operations parse it strictly and refuse a file that breaks any
/// rule.
pub trait Ledger {
    /// Why the ledger repository could not be read or written.
    type Error: std::error::Error;

    /// The file at `path` and its version; `None` when there is none.
    ///
    /// An implementation must refuse a file over [`MAX_LEDGER_FILE_BYTES`] without reading it
    /// whole.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the file cannot be read, or is over the cap.
    fn read(&self, path: &LedgerPath) -> Result<Option<LedgerFile>, Self::Error>;

    /// Replaces the file at `path` with `contents` only when it still is as `condition` says:
    /// absent, or at the version read. Otherwise nothing is written and the outcome is
    /// [`WriteOutcome::Conflict`].
    ///
    /// The ledger operations re-apply their change after a conflict, so an implementation
    /// returns `Conflict` only when it knows this write is not in the file. When it cannot tell
    /// (an attempt went unanswered and the file has since moved), it returns an error.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the ledger cannot be written, or the write cannot be confirmed; the
    /// file may or may not have changed.
    fn write(
        &self,
        path: &LedgerPath,
        contents: &[u8],
        condition: &WriteCondition,
    ) -> Result<WriteOutcome, Self::Error>;
}

/// A file of the ledger repository. Built only from typed parts, so no path ever comes from
/// untrusted text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LedgerPath(String);

impl LedgerPath {
    /// `ledger/YYYY-MM.jsonl`: the cost records written in `month`.
    #[must_use]
    pub fn costs(month: LedgerMonth) -> Self {
        Self(format!("ledger/{month}.jsonl"))
    }

    /// `pushes/<repo_id>/<pr>.jsonl`: PR `pr`'s push records, written by `open`.
    #[must_use]
    pub fn pushes(repo_id: NonZeroU64, pr: NonZeroU64) -> Self {
        Self(format!("pushes/{repo_id}/{pr}.jsonl"))
    }

    /// `commands/<repo_id>/<pr>.jsonl`: PR `pr`'s command records, written by decide-only.
    #[must_use]
    pub fn commands(repo_id: NonZeroU64, pr: NonZeroU64) -> Self {
        Self(format!("commands/{repo_id}/{pr}.jsonl"))
    }

    /// `state/<repo_id>/<pr>.json`: PR `pr`'s authoritative state (ADR-23).
    #[must_use]
    pub fn state(repo_id: NonZeroU64, pr: NonZeroU64) -> Self {
        Self(format!("state/{repo_id}/{pr}.json"))
    }

    /// The path from the repository root, `/`-separated, with no leading slash. Only ASCII
    /// letters, digits, `-`, `.` and `/` occur in it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LedgerPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The version of a ledger file a write is conditioned on: the adapter's own token for it (the
/// git blob sha for the GitHub contents API). Opaque to the application.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileVersion(String);

impl FileVersion {
    /// The version an adapter read.
    #[must_use]
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    /// The version as the adapter wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A ledger file as read: its version and its whole contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerFile {
    /// What a write over it is conditioned on.
    pub version: FileVersion,
    /// Its bytes, untrusted.
    pub contents: Vec<u8>,
}

/// What a [`Ledger::write`] requires of the file before it replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteCondition {
    /// There is no file yet.
    Absent,
    /// The file is still at this version.
    At(FileVersion),
}

/// What a [`Ledger::write`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteOutcome {
    /// The file now holds the contents given.
    Written,
    /// The file was not as the condition said; nothing was written.
    Conflict,
}
