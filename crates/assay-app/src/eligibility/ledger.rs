//! [`SnapshotLedger`] over the ledger's per-PR record streams (D15, #58): the push records
//! `open` appends and the command records decide-only appends, read by #70's strict readers.

use std::num::NonZeroU64;

use thiserror::Error;

use super::commands::SnapshotCommand;
use super::ports::{LedgerRecords, SnapshotLedger};
use crate::ledger::{Ledger, LedgerReadError, PushRecord, read_commands, read_pushes};
use crate::publish::CommentId;

/// The snapshot's ledger port for one repository.
///
/// A listing holds at most `max` records; past it the adapter answers
/// [`LedgerRecords::TooMany`] having kept at most `max + 1` (H-13). The ledger's own reader bounds
/// the file it reads (`MAX_LEDGER_FILE_BYTES`).
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
    /// A command record does not say which head it was posted on: `assay.ledger-command/1`
    /// has no `head_sha`, and a head-bound command cannot be judged without one, so the record
    /// is refused rather than given a head (wi-b5affa).
    #[error("command record of comment {} does not record its head", comment_id.0)]
    HeadUnrecorded {
        /// The comment the record came from.
        comment_id: CommentId,
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
        match records.first() {
            Some(record) => Err(LedgerSnapshotError::HeadUnrecorded {
                comment_id: record.comment_id,
            }),
            None => Ok(LedgerRecords::Listed(Vec::new())),
        }
    }
}
