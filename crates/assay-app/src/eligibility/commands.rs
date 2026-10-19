//! Commands are events, not state (04 §5, R3-02, T21): decide-only parses a command only from
//! the `issue_comment.created` payload that triggered it and records it; every later `decide`
//! reads the records, re-fetches each comment, and voids a record whose comment was deleted,
//! edited or no longer hashes to it. An edit can only revoke a command, never grant one.

use super::grammar::{Command, Parsed, parse_command};
use super::rule::{Ineligible, Rule};
use super::timestamp::Timestamp;
use crate::config::{CommitId, Sha256Digest};
use crate::publish::CommentId;
use crate::status::UserId;

/// The most command records read for one PR (H-13); more is refused.
pub const MAX_COMMAND_RECORDS: usize = 256;

/// An App-authored record of `commands/<repo_id>/<pr>.jsonl` as the snapshot judges it.
///
/// The command is parsed and bound to the head it was posted on. The ledger's own model of the
/// line is [`crate::CommandRecord`] (`assay.ledger-command/1`), which does not yet carry
/// `head_sha`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCommand {
    /// The comment the command was posted in.
    pub comment_id: CommentId,
    /// Who posted it, eligible when it was recorded.
    pub actor: UserId,
    /// The comment's `created_at`.
    pub created_at: Timestamp,
    /// The PR head it was posted on.
    pub head_sha: CommitId,
    /// The sha256 of the comment body, `sha256:<hex>`.
    pub body_sha256: Sha256Digest,
    /// The command.
    pub command: Command,
}

/// An issue comment as GitHub returns it. Its body is untrusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueComment {
    /// Its id.
    pub id: CommentId,
    /// Its author; `None` for a deleted account.
    pub user: Option<UserId>,
    /// When it was posted.
    pub created_at: Timestamp,
    /// When it was last changed.
    pub updated_at: Timestamp,
    /// Its body.
    pub body: String,
    /// Whether `performed_via_github_app` is not null: an app posted it for the user.
    pub via_app: bool,
}

/// What happened to a comment, as the triggering `issue_comment` event says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommentAction {
    /// `created`: the only action a command is read from.
    Created,
    /// `edited`.
    Edited,
    /// `deleted`.
    Deleted,
}

/// The triggering `issue_comment` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentEvent {
    /// Its action.
    pub action: CommentAction,
    /// The comment in its payload.
    pub comment: IssueComment,
}

/// What [`GitHubState::record_command`](super::GitHubState::record_command) made of a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// A privileged command to append to the ledger.
    Record(SnapshotCommand),
    /// `/assay recheck`: re-decide, record nothing.
    Recheck,
}

/// Why a comment is not recorded as a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum CommandRejected {
    /// The event is not `created`: an edit or a deletion never makes a command.
    #[error("only a newly created comment is read as a command")]
    NotCreated,
    /// An app posted the comment (`performed_via_github_app` is set).
    #[error("a comment posted through an app is not a command")]
    ViaApp,
    /// The comment has no author.
    #[error("the comment has no author")]
    NoAuthor,
    /// The first line does not start with `/assay`.
    #[error("not a command")]
    NotACommand,
    /// `/assay` names no command of the grammar.
    #[error("unknown command")]
    Unknown,
    /// The command's arguments do not parse.
    #[error("malformed command")]
    Malformed,
    /// The author may take no privileged action on this head.
    #[error("the author is not eligible: {0}")]
    Ineligible(Ineligible),
    /// The head has no push record, so no command can postdate its push.
    #[error("the head has no push record")]
    NoPushClock,
    /// The comment is not later than the head's push clock.
    #[error("the command predates the push of this head")]
    BeforePushClock,
}

/// Why a command record no longer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoidCause {
    /// The comment was deleted.
    Deleted,
    /// The comment was edited (`updated_at != created_at`), or its `created_at` changed.
    Edited,
    /// The comment's body no longer hashes to the record.
    Hash,
    /// The comment's author is not the record's actor.
    Author,
    /// The comment now says an app posted it.
    App,
}

impl VoidCause {
    /// The cause's code in the bundle.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::Edited => "edited",
            Self::Hash => "hash",
            Self::Author => "author",
            Self::App => "app",
        }
    }
}

/// A command record that does not apply to this decision, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandStatus {
    /// It applies.
    Valid,
    /// Its comment no longer stands.
    Void(VoidCause),
    /// A head-bound command posted on another head.
    OtherHead,
    /// Its actor is not eligible now.
    Ineligible(Ineligible),
    /// A head-bound command not later than the head's push clock (or with no clock).
    BeforePushClock,
}

/// A record with what this decision made of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JudgedCommand {
    pub(crate) record: SnapshotCommand,
    pub(crate) status: CommandStatus,
}

/// How a record's comment stands now: `None` when it still reads as recorded.
pub(crate) fn void_cause(
    record: &SnapshotCommand,
    comment: Option<&IssueComment>,
) -> Option<VoidCause> {
    let Some(comment) = comment else {
        return Some(VoidCause::Deleted);
    };
    if comment.updated_at != comment.created_at || comment.created_at != record.created_at {
        return Some(VoidCause::Edited);
    }
    if comment.user != Some(record.actor) {
        return Some(VoidCause::Author);
    }
    if comment.via_app {
        return Some(VoidCause::App);
    }
    (Sha256Digest::of(comment.body.as_bytes()) != record.body_sha256).then_some(VoidCause::Hash)
}

/// What a record that still stands is worth on `head`, with push clock `clock`.
///
/// A dismissal lives for the PR and its eligibility was judged when it was recorded, so only
/// the lists are re-read (a revoked owner's dismissal falls at once); a head-bound command must
/// name this head, postdate its push and come from an id eligible on it now.
pub(crate) fn applicability(
    record: &SnapshotCommand,
    head: &CommitId,
    clock: Option<Timestamp>,
    rule: &Rule,
) -> CommandStatus {
    if !record.command.is_head_bound() {
        return match rule.judge_lists(record.actor) {
            Ok(()) => CommandStatus::Valid,
            Err(why) => CommandStatus::Ineligible(why),
        };
    }
    if record.head_sha != *head {
        return CommandStatus::OtherHead;
    }
    if let Err(why) = rule.judge(record.actor) {
        return CommandStatus::Ineligible(why);
    }
    match clock {
        Some(clock) if record.created_at > clock => CommandStatus::Valid,
        Some(_) | None => CommandStatus::BeforePushClock,
    }
}

/// The record decide-only appends for the triggering `event` on `head`, or why there is none.
pub(crate) fn record(
    event: &CommentEvent,
    head: &CommitId,
    clock: Option<Timestamp>,
    rule: &Rule,
) -> Result<Recorded, CommandRejected> {
    let comment = &event.comment;
    if event.action != CommentAction::Created {
        return Err(CommandRejected::NotCreated);
    }
    if comment.via_app {
        return Err(CommandRejected::ViaApp);
    }
    let actor = comment.user.ok_or(CommandRejected::NoAuthor)?;
    let command = match parse_command(&comment.body) {
        Parsed::NotACommand => return Err(CommandRejected::NotACommand),
        Parsed::Unknown => return Err(CommandRejected::Unknown),
        Parsed::Malformed => return Err(CommandRejected::Malformed),
        Parsed::Command(Command::Recheck) => return Ok(Recorded::Recheck),
        Parsed::Command(command) => command,
    };
    rule.judge(actor).map_err(CommandRejected::Ineligible)?;
    let clock = clock.ok_or(CommandRejected::NoPushClock)?;
    if comment.created_at <= clock {
        return Err(CommandRejected::BeforePushClock);
    }
    Ok(Recorded::Record(SnapshotCommand {
        comment_id: comment.id,
        actor,
        created_at: comment.created_at,
        head_sha: head.clone(),
        body_sha256: Sha256Digest::of(comment.body.as_bytes()),
        command,
    }))
}
