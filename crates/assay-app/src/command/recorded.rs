//! The command a ledger record holds (`assay.ledger-command/1`): a command of the grammar other
//! than `recheck`, with its canonical line. It is checked once, where it enters (a judged
//! comment, a ledger line, or a command built by hand, whose line must read back as it), so a
//! record never holds a command the snapshot cannot judge (#353 F4).

use std::str::FromStr;

use super::{Command, Parsed, parse_command};

/// A command of the grammar other than `recheck`, held with its canonical [`Command::line`].
///
/// Built by [`RecordedCommand::new`] from a command whose line reads back as it, or by
/// [`FromStr`] from exactly that line, the form a command record holds.
///
/// # Examples
///
/// ```
/// use assay_app::{Command, RecordedCommand};
///
/// let waive: RecordedCommand = "/assay waive 0123456789abcdef".parse()?;
///
/// assert!(matches!(waive.as_command(), Command::Waive { .. }));
/// assert_eq!(waive.as_str(), "/assay waive 0123456789abcdef");
/// assert_eq!(RecordedCommand::new(Command::Recheck), None);
/// # Ok::<(), assay_app::ParseRecordedCommandError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordedCommand {
    /// The command; never [`Command::Recheck`].
    command: Command,
    /// `command.line()`, kept so the record's writer and readers borrow it.
    line: String,
}

impl RecordedCommand {
    /// `command` as a record holds it.
    ///
    /// `None` for [`Command::Recheck`], which is never recorded, and for a command whose line
    /// [`parse_command`] does not read back as it: [`Command`]'s fields are public, so a command
    /// built by hand may hold a reason the grammar refuses (empty, padded, over
    /// [`MAX_COMMAND_REASON_CHARS`](crate::MAX_COMMAND_REASON_CHARS) or with a control
    /// character). Every command [`parse_command`] gives, but `recheck`, is accepted.
    #[must_use]
    pub fn new(command: Command) -> Option<Self> {
        if matches!(command, Command::Recheck) {
            return None;
        }
        let line = command.line();
        let reads_back = matches!(parse_command(&line), Parsed::Command(read) if read == command);
        reads_back.then_some(Self { command, line })
    }

    /// The command.
    #[must_use]
    pub const fn as_command(&self) -> &Command {
        &self.command
    }

    /// Its canonical line (`/assay waive 0123456789abcdef`), the text a command record holds.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.line
    }
}

/// Why a text is not a [`RecordedCommand`]'s canonical line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ParseRecordedCommandError {
    /// It is not a command of the grammar.
    #[error("the recorded command is not a command of the grammar")]
    Unparsed,
    /// It is `/assay recheck`, which is never recorded.
    #[error("the recorded command is a recheck, which is never recorded")]
    Recheck,
    /// It reads as a command, but is not that command's canonical line.
    #[error("the recorded command is not its command's canonical line")]
    NotCanonical,
}

impl FromStr for RecordedCommand {
    type Err = ParseRecordedCommandError;

    /// The command `text` holds when it is exactly a non-`recheck` command's canonical line.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let command = match parse_command(text) {
            Parsed::Command(Command::Recheck) => return Err(ParseRecordedCommandError::Recheck),
            Parsed::Command(command) => command,
            Parsed::NotACommand | Parsed::Unknown | Parsed::Malformed => {
                return Err(ParseRecordedCommandError::Unparsed);
            }
        };
        // A command the grammar gave reads back from its own line, so `new` refuses none here;
        // a refusal would still be no canonical line.
        match Self::new(command) {
            Some(recorded) if recorded.line == text => Ok(recorded),
            Some(_) | None => Err(ParseRecordedCommandError::NotCanonical),
        }
    }
}
