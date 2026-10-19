//! Read, change, write by compare-and-set, and on a conflict read again: the one write loop of
//! every ledger operation, bounded by [`MAX_CAS_ATTEMPTS`] (06 §1).

use std::borrow::Cow;

use super::error::{Corruption, LedgerReadError, LedgerWriteError};
use super::lines::Refusal;
use super::port::{
    Ledger, LedgerFile, LedgerPath, MAX_CAS_ATTEMPTS, MAX_LEDGER_FILE_BYTES, WriteCondition,
    WriteOutcome,
};

/// What a change makes of the file it was shown.
pub(super) enum Step<T> {
    /// Nothing to write; the operation's result is `T`.
    Keep(T),
    /// Write these contents; once written, the operation's result is `T`.
    Write(Vec<u8>, T),
}

/// Why a change refused to go on.
#[derive(Clone, Copy)]
pub(super) enum Refused {
    /// The current file breaks a rule.
    Current(Refusal),
    /// The contents the change would write break a rule.
    New(Corruption),
}

/// The file at `path`, refused over [`MAX_LEDGER_FILE_BYTES`].
pub(super) fn read_file<L: Ledger>(
    ledger: &L,
    path: &LedgerPath,
) -> Result<Option<LedgerFile>, LedgerReadError<L::Error>> {
    let file = ledger.read(path).map_err(LedgerReadError::Ledger)?;
    if let Some(found) = &file
        && found.contents.len() > MAX_LEDGER_FILE_BYTES
    {
        return Err(LedgerReadError::TooLarge {
            path: path.clone(),
            size: found.contents.len(),
        });
    }
    Ok(file)
}

/// Runs `change` on the current contents of `path` (`None` when absent) and writes what it
/// returns, conditioned on the version read; on a conflict, reads again and re-runs `change`,
/// at most [`MAX_CAS_ATTEMPTS`] times.
///
/// `change` must derive everything it writes from the contents it is shown, so a retry applies
/// the change to the file as it now is and no other writer's change is lost.
pub(super) fn compare_and_set<L: Ledger, T>(
    ledger: &L,
    path: &LedgerPath,
    mut change: impl FnMut(Option<&[u8]>) -> Result<Step<T>, Refused>,
) -> Result<T, LedgerWriteError<L::Error>> {
    for _ in 0..MAX_CAS_ATTEMPTS {
        let file = read_file(ledger, path).map_err(LedgerWriteError::Read)?;
        let current = file.as_ref().map(|found| found.contents.as_slice());
        let (contents, result) = match change(current) {
            Ok(Step::Keep(result)) => return Ok(result),
            Ok(Step::Write(contents, result)) => (contents, result),
            Err(refused) => return Err(refusal(path, refused)),
        };
        if contents.len() > MAX_LEDGER_FILE_BYTES {
            return Err(LedgerWriteError::Full {
                path: path.clone(),
                size: contents.len(),
            });
        }
        let condition = file.map_or(WriteCondition::Absent, |found| {
            WriteCondition::At(found.version)
        });
        match ledger
            .write(path, &contents, &condition)
            .map_err(LedgerWriteError::Write)?
        {
            WriteOutcome::Written => return Ok(result),
            WriteOutcome::Conflict => {}
        }
    }
    Err(LedgerWriteError::Contended {
        path: path.clone(),
        attempts: MAX_CAS_ATTEMPTS,
    })
}

/// What an append makes of a stream's records.
pub(super) enum Next<'line, T> {
    /// Nothing to append; the operation's result is `T`.
    Keep(T),
    /// Append this line (newline included); once written, the operation's result is `T`.
    Append(Cow<'line, [u8]>, T),
}

/// Appends to the record stream at `path` what `next` decides from the records already there,
/// by [`compare_and_set`]. Both the current file and the file it would write are read with
/// `parse`, so a stream is never written in a form its own reader refuses.
pub(super) fn append_line<'line, L: Ledger, R, T>(
    ledger: &L,
    path: &LedgerPath,
    parse: impl Fn(&[u8]) -> Result<Vec<R>, Refusal>,
    mut next: impl FnMut(Vec<R>) -> Result<Next<'line, T>, Corruption>,
) -> Result<T, LedgerWriteError<L::Error>> {
    compare_and_set(ledger, path, |current| {
        // An absent file holds no record yet.
        let current = current.unwrap_or_default();
        let records = parse(current).map_err(Refused::Current)?;
        let (line, result) = match next(records).map_err(Refused::New)? {
            Next::Keep(result) => return Ok(Step::Keep(result)),
            Next::Append(line, result) => (line, result),
        };
        let mut contents = Vec::with_capacity(current.len().saturating_add(line.len()));
        contents.extend_from_slice(current);
        contents.extend_from_slice(&line);
        parse(&contents).map_err(|(_line, problem)| Refused::New(problem))?;
        Ok(Step::Write(contents, result))
    })
}

/// The write error for a refused change of `path`.
fn refusal<E>(path: &LedgerPath, refused: Refused) -> LedgerWriteError<E> {
    match refused {
        Refused::Current((line, problem)) => LedgerWriteError::Read(LedgerReadError::Corrupt {
            path: path.clone(),
            line,
            problem,
        }),
        Refused::New(problem) => LedgerWriteError::Invalid {
            path: path.clone(),
            problem,
        },
    }
}
