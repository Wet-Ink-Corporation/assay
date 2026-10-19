//! Command records, `assay.ledger-command/1`: `commands/<repo_id>/<pr>.jsonl`, appended by
//! decide-only from the triggering `issue_comment.created` payload, one per comment (06 §1, T21,
//! R3-02). `decide` reads these records, never comment bodies.

use std::borrow::Cow;
use std::collections::HashSet;
use std::num::NonZeroU64;

use assay_domain::RunUid;
use serde::{Deserialize, Serialize};

use super::cas::{Next, append_line, read_file};
use super::error::{Corruption, LedgerReadError, LedgerWriteError};
use super::lines::{Refusal, check_schema, lines, parse, to_line};
use super::port::{Ledger, LedgerPath};
use super::record::{Appended, field};
use super::time::LedgerTime;
use crate::config::Sha256Digest;
use crate::publish::CommentId;
use crate::status::UserId;

/// The contract id every command record carries.
const COMMAND_SCHEMA: &str = "assay.ledger-command/1";

/// The longest command a record holds, in characters.
pub const MAX_COMMAND_CHARS: usize = 256;

/// A command as decide-only accepted it from a comment's `created` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    /// The decide-only run that recorded it.
    pub run_uid: RunUid,
    /// The PR number.
    pub pr: NonZeroU64,
    /// The comment it came from; one record per comment.
    pub comment_id: CommentId,
    /// Who posted the comment, by numeric id.
    pub actor_id: UserId,
    /// The command line as parsed (`/assay recheck`): 1 to [`MAX_COMMAND_CHARS`] characters, no
    /// control character.
    pub command: String,
    /// The sha256 of the comment body at `created`: a later body with another hash voids the
    /// record (T21).
    pub body_sha256: Sha256Digest,
    /// The comment's `created_at`.
    pub created_at: LedgerTime,
    /// When decide-only recorded it.
    pub at: LedgerTime,
}

/// Appends `command` to its PR's command records, unless its comment already has one. When this
/// fails, the command has no effect and the reply says so (06 §1).
///
/// # Errors
///
/// [`LedgerWriteError`]: the file could not be read or is refused, the write failed or kept
/// conflicting, the file would pass its cap, or `command` breaks a rule of its contract (an empty
/// or over-long command, or one with a control character).
pub fn append_command<L: Ledger>(
    ledger: &L,
    command: &CommandRecord,
) -> Result<Appended<CommandRecord>, LedgerWriteError<L::Error>> {
    let (repo_id, pr) = (command.run_uid.repo_id, command.pr);
    let path = LedgerPath::commands(repo_id, pr);
    let line =
        to_line(&CommandLine::from(command)).map_err(|problem| LedgerWriteError::Invalid {
            path: path.clone(),
            problem,
        })?;
    append_line(
        ledger,
        &path,
        |contents| parse_commands(repo_id, pr, contents),
        |existing| {
            let found = existing
                .into_iter()
                .find(|found| found.comment_id == command.comment_id);
            Ok(match found {
                Some(found) => Next::Keep(Appended::AlreadyRecorded(found)),
                None => Next::Append(Cow::Borrowed(&line), Appended::Recorded),
            })
        },
    )
}

/// PR `pr`'s command records, oldest first; none when there is no file.
///
/// # Errors
///
/// [`LedgerReadError`] when the file cannot be read, is over its cap, or breaks a rule.
pub fn read_commands<L: Ledger>(
    ledger: &L,
    repo_id: NonZeroU64,
    pr: NonZeroU64,
) -> Result<Vec<CommandRecord>, LedgerReadError<L::Error>> {
    let path = LedgerPath::commands(repo_id, pr);
    let Some(file) = read_file(ledger, &path)? else {
        return Ok(Vec::new());
    };
    parse_commands(repo_id, pr, &file.contents).map_err(|(line, problem)| {
        LedgerReadError::Corrupt {
            path,
            line,
            problem,
        }
    })
}

/// The command records of PR `pr`'s file `contents`, refused whole when any line breaks a rule:
/// a record of another repository or PR, or a comment's second record.
fn parse_commands(
    repo_id: NonZeroU64,
    pr: NonZeroU64,
    contents: &[u8],
) -> Result<Vec<CommandRecord>, Refusal> {
    let lines = lines(contents)?;
    let mut comments = HashSet::with_capacity(lines.len());
    let mut records = Vec::with_capacity(lines.len());
    for (number, line) in lines {
        let refuse = |problem| (number, problem);
        let record = CommandRecord::try_from(parse::<CommandLine<'_>>(line).map_err(refuse)?)
            .map_err(refuse)?;
        if (record.run_uid.repo_id, record.pr) != (repo_id, pr) {
            return Err(refuse(Corruption::WrongPullRequest));
        }
        if !comments.insert(record.comment_id) {
            return Err(refuse(Corruption::Duplicate));
        }
        records.push(record);
    }
    Ok(records)
}

/// The wire form of a command record (OUT-01).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandLine<'a> {
    schema: Cow<'static, str>,
    run_uid: RunUid,
    pr: NonZeroU64,
    comment_id: NonZeroU64,
    actor_id: NonZeroU64,
    command: Cow<'a, str>,
    body_sha256: String,
    created_at: String,
    at: String,
}

impl<'a> From<&'a CommandRecord> for CommandLine<'a> {
    fn from(record: &'a CommandRecord) -> Self {
        Self {
            schema: Cow::Borrowed(COMMAND_SCHEMA),
            run_uid: record.run_uid,
            pr: record.pr,
            comment_id: record.comment_id.0,
            actor_id: record.actor_id.0,
            command: Cow::Borrowed(&record.command),
            body_sha256: record.body_sha256.to_string(),
            created_at: record.created_at.to_string(),
            at: record.at.to_string(),
        }
    }
}

impl TryFrom<CommandLine<'_>> for CommandRecord {
    type Error = Corruption;

    fn try_from(line: CommandLine<'_>) -> Result<Self, Corruption> {
        check_schema(&line.schema, COMMAND_SCHEMA)?;
        let chars = line.command.chars().count();
        if !(1..=MAX_COMMAND_CHARS).contains(&chars) || line.command.chars().any(char::is_control) {
            return Err(Corruption::Field("command"));
        }
        Ok(Self {
            run_uid: line.run_uid,
            pr: line.pr,
            comment_id: CommentId(line.comment_id),
            actor_id: UserId(line.actor_id),
            command: line.command.into_owned(),
            body_sha256: field(&line.body_sha256, "body_sha256")?,
            created_at: field(&line.created_at, "created_at")?,
            at: field(&line.at, "at")?,
        })
    }
}
