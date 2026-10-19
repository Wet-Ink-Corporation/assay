//! The closed command grammar of 04 §6 (R3-24): one command per comment, on its first line,
//! exact tokens separated by single spaces. A first line that does not start with `/assay`
//! (a blockquote `>`, a fence, indentation, other text) is not a command, so a quote-reply of
//! agent text never is one.

use assay_domain::{Fingerprint, OverrideScope, RequiredRole};

use crate::config::Sha256Digest;

/// The longest reason a command may carry, in characters (H-13).
pub const MAX_COMMAND_REASON_CHARS: usize = 200;

/// The command word every command starts with.
const PREFIX: &str = "/assay";

/// One command of the closed grammar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Command {
    /// `/assay recheck`: re-decide; not privileged, never recorded.
    Recheck,
    /// `/assay waive <finding-id>`.
    Waive {
        /// The gate finding's fingerprint.
        finding: Fingerprint,
    },
    /// `/assay override llm|all <reasons_digest> <reason>`.
    Override {
        /// What it neutralises.
        scope: OverrideScope,
        /// The reasons digest it quotes.
        reasons_digest: Sha256Digest,
        /// Why.
        reason: String,
    },
    /// `/assay dismiss <fp> <reason>`.
    Dismiss {
        /// The reviewer finding's fingerprint.
        finding: Fingerprint,
        /// Why.
        reason: String,
    },
    /// `/assay retry <role>`.
    Retry {
        /// The degraded role.
        role: RequiredRole,
    },
    /// `/assay weigh-in-ack <set_digest>|none`.
    WeighInAck {
        /// The blocking set acknowledged.
        set: AckedSet,
    },
}

/// What a Weigh-In ack acknowledges.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AckedSet {
    /// The blocking set with this digest.
    Digest(Sha256Digest),
    /// `none`: the PR needs no journal (04 §9 rule 3).
    None,
}

/// What a comment's first line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// It does not start with `/assay`.
    NotACommand,
    /// A command of the grammar.
    Command(Command),
    /// `/assay` followed by a word that names no command: rejected with a reply.
    Unknown,
    /// A known command whose arguments do not parse.
    Malformed,
}

impl Command {
    /// The command's word (`waive`).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Recheck => "recheck",
            Self::Waive { .. } => "waive",
            Self::Override { .. } => "override",
            Self::Dismiss { .. } => "dismiss",
            Self::Retry { .. } => "retry",
            Self::WeighInAck { .. } => "weigh-in-ack",
        }
    }

    /// The command's arguments as written, the reason last.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::Recheck => Vec::new(),
            Self::Waive { finding } => vec![finding.to_string()],
            Self::Override {
                scope,
                reasons_digest,
                reason,
            } => vec![
                scope.code().to_owned(),
                reasons_digest.as_str().to_owned(),
                reason.clone(),
            ],
            Self::Dismiss { finding, reason } => vec![finding.to_string(), reason.clone()],
            Self::Retry { role } => vec![role.code().to_owned()],
            Self::WeighInAck { set } => vec![match set {
                AckedSet::Digest(digest) => digest.as_str().to_owned(),
                AckedSet::None => "none".to_owned(),
            }],
        }
    }

    /// Whether the command's lifetime is the head (all but dismiss and recheck, 04 §5).
    #[must_use]
    pub const fn is_head_bound(&self) -> bool {
        matches!(
            self,
            Self::Waive { .. }
                | Self::Override { .. }
                | Self::Retry { .. }
                | Self::WeighInAck { .. }
        )
    }
}

/// Reads `body`'s first line as a command.
///
/// # Examples
///
/// ```
/// use assay_app::{Command, Parsed, parse_command};
///
/// assert_eq!(parse_command("/assay recheck\nthanks"), Parsed::Command(Command::Recheck));
/// assert_eq!(parse_command("> /assay recheck"), Parsed::NotACommand);
/// assert_eq!(parse_command("/assay approve"), Parsed::Unknown);
/// ```
#[must_use]
pub fn parse_command(body: &str) -> Parsed {
    let Some(rest) = command_line(body) else {
        return Parsed::NotACommand;
    };
    let (word, args) = rest.split_once(' ').unwrap_or((rest, ""));
    let parsed = match word {
        "recheck" => args.is_empty().then_some(Command::Recheck),
        "waive" => fingerprint(args).map(|finding| Command::Waive { finding }),
        "override" => override_command(args),
        "dismiss" => args.split_once(' ').and_then(|(finding, reason)| {
            Some(Command::Dismiss {
                finding: fingerprint(finding)?,
                reason: reason_text(reason)?,
            })
        }),
        "retry" => RequiredRole::ALL
            .into_iter()
            .find(|role| role.code() == args)
            .map(|role| Command::Retry { role }),
        "weigh-in-ack" => acked_set(args).map(|set| Command::WeighInAck { set }),
        _ => return Parsed::Unknown,
    };
    parsed.map_or(Parsed::Malformed, Parsed::Command)
}

/// What follows `/assay ` on `body`'s first line (empty for a bare `/assay`), or `None` when
/// that line is not a command line.
fn command_line(body: &str) -> Option<&str> {
    let line = body.split_once('\n').map_or(body, |(first, _rest)| first);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let rest = line.strip_prefix(PREFIX)?;
    if rest.is_empty() {
        return Some(rest);
    }
    rest.strip_prefix(' ')
}

/// The grammar's command word on `body`'s first line, for events: the word when the grammar
/// has it, `unknown` for any other word, and `None` when the line is not a command line. Never
/// the comment's own text.
pub(crate) fn command_word(body: &str) -> Option<&'static str> {
    let rest = command_line(body)?;
    let word = rest.split_once(' ').map_or(rest, |(word, _args)| word);
    Some(
        WORDS
            .into_iter()
            .find(|known| *known == word)
            .unwrap_or("unknown"),
    )
}

/// Every command word of the grammar, as [`Command::name`] gives it.
const WORDS: [&str; 6] = [
    "recheck",
    "waive",
    "override",
    "dismiss",
    "retry",
    "weigh-in-ack",
];

/// `llm|all <reasons_digest> <reason>`.
fn override_command(args: &str) -> Option<Command> {
    let (scope, rest) = args.split_once(' ')?;
    let (digest, reason) = rest.split_once(' ')?;
    Some(Command::Override {
        scope: OverrideScope::ALL
            .into_iter()
            .find(|candidate| candidate.code() == scope)?,
        reasons_digest: digest.parse().ok()?,
        reason: reason_text(reason)?,
    })
}

/// A fingerprint written exactly, nothing after it.
fn fingerprint(token: &str) -> Option<Fingerprint> {
    token.parse().ok()
}

/// `<set_digest>` or `none`.
fn acked_set(token: &str) -> Option<AckedSet> {
    if token == "none" {
        return Some(AckedSet::None);
    }
    token.parse().ok().map(AckedSet::Digest)
}

/// A reason: not empty, not starting or ending with a space, at most
/// [`MAX_COMMAND_REASON_CHARS`] characters, no control character.
fn reason_text(reason: &str) -> Option<String> {
    let fits = !reason.is_empty()
        && reason.trim() == reason
        && reason.chars().count() <= MAX_COMMAND_REASON_CHARS
        && !reason.chars().any(char::is_control);
    fits.then(|| reason.to_owned())
}
