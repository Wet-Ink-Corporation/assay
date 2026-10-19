//! Why a ledger read or write failed: the port's own failure apart from a file assay refuses to
//! interpret (ERR-10, wi-b5affa). No error quotes ledger contents.

use thiserror::Error;

use super::port::{LedgerPath, MAX_LEDGER_FILE_BYTES};

/// Why a ledger file could not be read. Every variant leaves the caller without the data: an
/// unreadable ledger fails `admit`, so the LLM-unavailable policy applies (06 §1, F16).
#[derive(Debug, Error)]
pub enum LedgerReadError<E> {
    /// The ledger repository could not be read.
    #[error("the ledger could not be read")]
    Ledger(#[source] E),
    /// The file is longer than [`MAX_LEDGER_FILE_BYTES`]; it was not parsed.
    #[error("ledger file {path} is {size} bytes, over the {MAX_LEDGER_FILE_BYTES}-byte cap")]
    TooLarge {
        /// The file.
        path: LedgerPath,
        /// Its length in bytes.
        size: usize,
    },
    /// The file breaks a rule of its contract; none of it is used.
    #[error("ledger file {path} is refused at line {line}: {problem}")]
    Corrupt {
        /// The file.
        path: LedgerPath,
        /// The first line that breaks a rule, from 1 (1 for a JSON document).
        line: usize,
        /// The rule it breaks.
        problem: Corruption,
    },
}

/// Why a ledger file was not written.
///
/// Callers that only warn on an unwritable ledger (`release`, `decide`, F16) and callers that
/// must stop (`open`'s push record, decide-only's command record, 06 §1) both see every case.
#[derive(Debug, Error)]
pub enum LedgerWriteError<E> {
    /// The file could not be read before writing it, so nothing was written.
    #[error(transparent)]
    Read(LedgerReadError<E>),
    /// The ledger repository could not be written; the file may or may not have changed.
    #[error("the ledger could not be written")]
    Write(#[source] E),
    /// The file changed under every attempt; nothing was written.
    #[error("ledger file {path} kept changing: {attempts} attempts conflicted")]
    Contended {
        /// The file.
        path: LedgerPath,
        /// How many writes conflicted.
        attempts: u8,
    },
    /// The file would pass [`MAX_LEDGER_FILE_BYTES`]; nothing was written.
    #[error("ledger file {path} would be {size} bytes, over the {MAX_LEDGER_FILE_BYTES}-byte cap")]
    Full {
        /// The file.
        path: LedgerPath,
        /// The length it would have had.
        size: usize,
    },
    /// The file would break a rule its own reader enforces, so it was not written.
    #[error("ledger file {path} would be refused when read back: {problem}")]
    Invalid {
        /// The file.
        path: LedgerPath,
        /// The rule the written file would break.
        problem: Corruption,
    },
}

/// The rule a ledger file breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum Corruption {
    /// The last line of a record stream has no newline: a write was cut, or someone else wrote it.
    #[error("the last line is not terminated")]
    Unterminated,
    /// A record stream has an empty line.
    #[error("an empty line")]
    EmptyLine,
    /// A line is longer than [`MAX_RECORD_BYTES`](super::MAX_RECORD_BYTES).
    #[error("a line over the record cap")]
    LongLine,
    /// The text is not one JSON object of the contract's shape: not JSON, a repeated key, an
    /// unknown or missing field, or a mistyped value.
    #[error("not a record of the contract's shape")]
    Shape,
    /// The record reads, but not byte for byte as assay writes it (key order, spacing, escapes,
    /// number forms, an object written as an array): assay's writer is the only writer, so
    /// someone else wrote it.
    #[error("not in the form assay writes")]
    NotCanonical,
    /// The `schema` field names another contract or version.
    #[error("another contract or version")]
    Schema,
    /// This field's value is not one the contract allows.
    #[error("field {0} holds a value the contract does not allow")]
    Field(&'static str),
    /// A cost record whose `at` is not in the month its file is for.
    #[error("a record of another month")]
    WrongMonth,
    /// A record of another repository or PR than its file's.
    #[error("a record of another repository or PR")]
    WrongPullRequest,
    /// A second record with the key of an earlier one.
    #[error("a second record with the same key")]
    Duplicate,
    /// A push record whose `seq` is not one more than the previous one's (the first is 1).
    #[error("a push record out of sequence")]
    Sequence,
}
