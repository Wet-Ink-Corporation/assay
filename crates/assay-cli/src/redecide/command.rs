//! The triggering comment's command (J1, D7, D10, 06 §1): read from the payload only, judged
//! by the snapshot's rule, recorded in the ledger when it is privileged, and answered once with
//! its closed reason when it is not acted on.

use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    Appended, BundleSource, Command, CommandRejected, CommandReplies, CommentEvent, GitHubState,
    Ledger, LedgerTime, PullRequestState, Recorded, Reply, SnapshotCommand, TipLists,
    TriggerCommand, append_command,
};
use assay_domain::RunUid;

use super::{RedecideJobPorts, RedecideRunError, RedecideRunFailure};

/// What became of the command.
pub(super) enum Commanded {
    /// `/assay recheck`: decide over the snapshot as it is.
    Recheck,
    /// The command was appended to the ledger: decide with it added to the snapshot.
    Recorded(SnapshotCommand),
    /// The command was answered and is not acted on: nothing to publish (D10).
    Stop,
}

/// Judges the triggering `event` over `github` and records or answers it.
///
/// `/assay recheck` decides as it is. `/assay retry` is answered that it is not available yet
/// and recorded nowhere (D7). Any other command the rule accepts is appended to the ledger with
/// the live head (an earlier record of the same comment is accepted); one that cannot be
/// appended is answered that it has no effect, and is an error. A rejected command is answered
/// with its closed reason, unless it is one no reply is owed for.
pub(super) fn settle_command<A, R, L, G, E>(
    ports: &RedecideJobPorts<'_, A, R, L, G>,
    (run_uid, pr, at): (RunUid, NonZeroU64, LedgerTime),
    event: &CommentEvent,
    github: &GitHubState,
) -> Result<Commanded, RedecideRunFailure<A, R, L>>
where
    A: PullRequestState<Error = E> + CommandReplies<Error = E> + TipLists,
    R: BundleSource,
    L: Ledger<Error: 'static>,
    E: Error,
{
    let to = event.comment.id;
    let stop = |reply| {
        ports
            .app
            .reply(pr, to, reply)
            .map(|()| Commanded::Stop)
            .map_err(RedecideRunError::Reply)
    };
    let command = match github.record_command(event) {
        Ok(Recorded::Recheck) => return Ok(Commanded::Recheck),
        Ok(Recorded::Record(command)) => command,
        Err(reason) if answered(reason) => return stop(Reply::Rejected(reason)),
        Err(_unanswered) => return Ok(Commanded::Stop),
    };
    if matches!(command.command, Command::Retry { .. }) {
        return stop(Reply::RetryUnavailable);
    }
    let record = command
        .record(run_uid, pr, at)
        .map_err(RedecideRunError::Record)?;
    match append_command(ports.ledger, &record) {
        Ok(Appended::Recorded | Appended::AlreadyRecorded(_)) => {}
        Err(source) => {
            let reply = ports.app.reply(pr, to, Reply::NotRecorded).err();
            return Err(RedecideRunError::Append { source, reply });
        }
    }
    Ok(Commanded::Recorded(command))
}

/// Whether a rejection is answered (D10): only an `/assay` line a person posted on a new
/// comment, so not a non-command, an edit or deletion, or a comment with no author.
const fn answered(reason: CommandRejected) -> bool {
    !matches!(
        reason,
        CommandRejected::NotACommand | CommandRejected::NotCreated | CommandRejected::NoAuthor
    )
}

/// The triggering command as the decision bundle's first event records it: the one recorded,
/// or none for a recheck, a label or the poll.
pub(super) fn trigger_command(recorded: Option<&SnapshotCommand>) -> Option<TriggerCommand<'_>> {
    recorded.map(TriggerCommand::Recorded)
}
