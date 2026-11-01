//! [`SnapshotLedger`] over the ledger's per-PR record streams (D15, #58): the push records
//! `open` appends and the command records decide-only appends, read by #70's strict readers.

use std::num::NonZeroU64;

use thiserror::Error;

use super::commands::{SnapshotCommand, UnreadCommand};
use super::ports::{LedgerRecords, SnapshotLedger};
use crate::ledger::{Ledger, LedgerReadError, PushRecord, read_commands, read_pushes};
use crate::publish::CommentId;

/// The snapshot's ledger port for one repository.
///
/// A listing holds at most `max` records; past it the adapter answers
/// [`LedgerRecords::TooMany`]. The ledger's strict reader parses every record of the stream
/// before the count is compared, so what bounds the memory it uses is the file's size, refused
/// over [`MAX_LEDGER_FILE_BYTES`](crate::MAX_LEDGER_FILE_BYTES) unparsed (H-13), not `max`.
#[derive(Debug)]
pub struct LedgerSnapshot<'a, L> {
    /// The ledger's files.
    pub ledger: &'a L,
    /// The repository whose PRs it reads.
    pub repo_id: NonZeroU64,
}

/// Why [`LedgerSnapshot`] could not list a PR's records.
#[derive(Debug, Error)]
pub enum LedgerSnapshotError<E> {
    /// The record stream could not be read or is refused.
    #[error("could not read the PR's ledger records")]
    Read(#[source] LedgerReadError<E>),
    /// A command record's command is not one the snapshot can judge. The strict reader refuses
    /// such a line first, so this guards the conversion rather than the file (wi-b5affa).
    #[error("command record of comment {} cannot be judged", comment_id.0)]
    Command {
        /// The comment the record came from.
        comment_id: CommentId,
        /// What is wrong with its command.
        #[source]
        problem: UnreadCommand,
    },
}

// `Error::source` hands out `&(dyn Error + 'static)`, so a chained cause must be `'static`.
impl<L: Ledger> SnapshotLedger for LedgerSnapshot<'_, L>
where
    L::Error: 'static,
{
    type Error = LedgerSnapshotError<L::Error>;

    fn push_records(
        &self,
        number: NonZeroU64,
        max: usize,
    ) -> Result<LedgerRecords<PushRecord>, Self::Error> {
        let records =
            read_pushes(self.ledger, self.repo_id, number).map_err(LedgerSnapshotError::Read)?;
        Ok(if records.len() > max {
            LedgerRecords::TooMany
        } else {
            LedgerRecords::Listed(records)
        })
    }

    fn command_records(
        &self,
        number: NonZeroU64,
        max: usize,
    ) -> Result<LedgerRecords<SnapshotCommand>, Self::Error> {
        let records =
            read_commands(self.ledger, self.repo_id, number).map_err(LedgerSnapshotError::Read)?;
        if records.len() > max {
            return Ok(LedgerRecords::TooMany);
        }
        records
            .iter()
            .map(|record| {
                SnapshotCommand::try_from(record).map_err(|problem| LedgerSnapshotError::Command {
                    comment_id: record.comment_id,
                    problem,
                })
            })
            .collect::<Result<_, _>>()
            .map(LedgerRecords::Listed)
    }
}
