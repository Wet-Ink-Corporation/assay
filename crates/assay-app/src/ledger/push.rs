//! Push records, `assay.ledger-push/1`: `pushes/<repo_id>/<pr>.jsonl`, appended by `open` on
//! each `synchronize`, numbered 1, 2, 3, ... (06 §1, R3-07). The latest record's `seq` goes in the
//! pending stamp and its `at` is the push clock (02 §5).

use std::borrow::Cow;
use std::collections::HashSet;
use std::num::NonZeroU64;

use assay_domain::RunUid;
use serde::{Deserialize, Serialize};

use super::cas::{Next, append_line, read_file};
use super::error::{Corruption, LedgerReadError, LedgerWriteError};
use super::lines::{Refusal, check_schema, lines, parse, to_line};
use super::port::{Ledger, LedgerPath};
use super::record::field;
use super::time::LedgerTime;
use crate::config::CommitId;
use crate::status::UserId;

/// The contract id every push record carries.
const PUSH_SCHEMA: &str = "assay.ledger-push/1";

/// A push as `open` saw it, before the ledger numbers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDraft {
    /// The `open` run that recorded it; one push record per run.
    pub run_uid: RunUid,
    /// The PR number.
    pub pr: NonZeroU64,
    /// The head before the push.
    pub before: CommitId,
    /// The head after it.
    pub after: CommitId,
    /// Who pushed, by numeric id: a pusher is never an eligible approver (P10, T9).
    pub sender_id: UserId,
    /// Whether the push rewrote history (`before` is not an ancestor of `after`).
    pub forced: bool,
    /// When `open` recorded it: the push clock approvals must be later than.
    pub at: LedgerTime,
}

/// A push record as the ledger holds it: the push and its number in the PR's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRecord {
    /// 1 for the PR's first record, one more for each next one.
    pub seq: NonZeroU64,
    /// The push.
    pub push: PushDraft,
}

/// What [`append_push`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushAppended {
    /// The push was written with the next number.
    Recorded(PushRecord),
    /// The run had already recorded its push, so nothing was written: the record there.
    AlreadyRecorded(PushRecord),
}

impl PushAppended {
    /// The run's push record, written now or before.
    #[must_use]
    pub const fn record(&self) -> &PushRecord {
        match self {
            Self::Recorded(record) | Self::AlreadyRecorded(record) => record,
        }
    }
}

/// Appends `push` to its PR's push records with the next `seq`, unless its run already recorded
/// one. When this fails, `open` aborts visibly (S0, 06 §1).
///
/// # Errors
///
/// [`LedgerWriteError`]: the file could not be read or is refused, the write failed or kept
/// conflicting, or the file would pass its cap.
pub fn append_push<L: Ledger>(
    ledger: &L,
    push: &PushDraft,
) -> Result<PushAppended, LedgerWriteError<L::Error>> {
    let (repo_id, pr) = (push.run_uid.repo_id, push.pr);
    let path = LedgerPath::pushes(repo_id, pr);
    append_line(
        ledger,
        &path,
        |contents| parse_pushes(repo_id, pr, contents),
        |existing| {
            let next_seq = existing
                .last()
                .map_or(Some(NonZeroU64::MIN), |last| last.seq.checked_add(1))
                .ok_or(Corruption::Sequence)?;
            if let Some(found) = existing
                .into_iter()
                .find(|found| found.push.run_uid == push.run_uid)
            {
                return Ok(Next::Keep(PushAppended::AlreadyRecorded(found)));
            }
            // The returned record owns its push; the caller keeps the draft it lent.
            let record = PushRecord {
                seq: next_seq,
                push: push.clone(),
            };
            let line = to_line(&PushLine::from(&record))?;
            Ok(Next::Append(
                Cow::Owned(line),
                PushAppended::Recorded(record),
            ))
        },
    )
}

/// PR `pr`'s push records, oldest first; none when there is no file.
///
/// # Errors
///
/// [`LedgerReadError`] when the file cannot be read, is over its cap, or breaks a rule.
pub fn read_pushes<L: Ledger>(
    ledger: &L,
    repo_id: NonZeroU64,
    pr: NonZeroU64,
) -> Result<Vec<PushRecord>, LedgerReadError<L::Error>> {
    let path = LedgerPath::pushes(repo_id, pr);
    let Some(file) = read_file(ledger, &path)? else {
        return Ok(Vec::new());
    };
    parse_pushes(repo_id, pr, &file.contents).map_err(|(line, problem)| LedgerReadError::Corrupt {
        path,
        line,
        problem,
    })
}

/// The push records of PR `pr`'s file `contents`, refused whole when any line breaks a rule: a
/// record of another repository or PR, a `seq` other than one more than the line before's, or a
/// run's second record.
fn parse_pushes(
    repo_id: NonZeroU64,
    pr: NonZeroU64,
    contents: &[u8],
) -> Result<Vec<PushRecord>, Refusal> {
    let lines = lines(contents)?;
    let mut runs = HashSet::with_capacity(lines.len());
    let mut records: Vec<PushRecord> = Vec::with_capacity(lines.len());
    for (number, line) in lines {
        let refuse = |problem| (number, problem);
        let record =
            PushRecord::try_from(parse::<PushLine>(line).map_err(refuse)?).map_err(refuse)?;
        if (record.push.run_uid.repo_id, record.push.pr) != (repo_id, pr) {
            return Err(refuse(Corruption::WrongPullRequest));
        }
        let expected = records
            .last()
            .map_or(Some(NonZeroU64::MIN), |last| last.seq.checked_add(1));
        if expected != Some(record.seq) {
            return Err(refuse(Corruption::Sequence));
        }
        if !runs.insert(record.push.run_uid) {
            return Err(refuse(Corruption::Duplicate));
        }
        records.push(record);
    }
    Ok(records)
}

/// The wire form of a push record (OUT-01).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PushLine {
    schema: Cow<'static, str>,
    run_uid: RunUid,
    pr: NonZeroU64,
    seq: NonZeroU64,
    before: String,
    after: String,
    sender_id: NonZeroU64,
    forced: bool,
    at: String,
}

impl From<&PushRecord> for PushLine {
    fn from(record: &PushRecord) -> Self {
        let push = &record.push;
        Self {
            schema: Cow::Borrowed(PUSH_SCHEMA),
            run_uid: push.run_uid,
            pr: push.pr,
            seq: record.seq,
            before: push.before.to_string(),
            after: push.after.to_string(),
            sender_id: push.sender_id.0,
            forced: push.forced,
            at: push.at.to_string(),
        }
    }
}

impl TryFrom<PushLine> for PushRecord {
    type Error = Corruption;

    fn try_from(line: PushLine) -> Result<Self, Corruption> {
        check_schema(&line.schema, PUSH_SCHEMA)?;
        Ok(Self {
            seq: line.seq,
            push: PushDraft {
                run_uid: line.run_uid,
                pr: line.pr,
                before: field(&line.before, "before")?,
                after: field(&line.after, "after")?,
                sender_id: UserId(line.sender_id),
                forced: line.forced,
                at: field(&line.at, "at")?,
            },
        })
    }
}
