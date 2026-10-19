//! `doctor`'s profile checks (`docs/architecture/03-contracts.md` §8, A-10, #84): the pinned CLI
//! is at least [`MIN_CLI_VERSION`], the preflight passes, and one canary call with the production
//! argv plus `--include-hook-events`, in a throwaway checkout planted with hooks at user, project
//! and in-tree scope, shows each flag honoured by observation rather than by trusting it:
//!
//! - the argv, `--restricted` with `--safe-mode` included, was accepted and answered;
//! - the init's `permissionMode` is `dontAsk`;
//! - the reply holds the word the stdin prompt gave and the word the appended system prompt
//!   file gave;
//! - no planted hook ran and no hook event is in the stream (the hook canary);
//! - `Read //proc/self/environ` is in `permission_denials` and the token is nowhere in the
//!   transcript (the fence canary);
//! - a file in the `--add-dir` tree was read (it counts as a working directory), and a file
//!   outside the working directories was denied and its word never seen.
//!
//! Every check fails closed: what it cannot see fails it.

mod flags;
mod judge;

use std::ffi::OsStr;
use std::fmt;

use thiserror::Error;

use crate::config::{BuiltinAllowlist, ExactVersion};
use crate::model_runner::ModelOutcome;
use crate::plan::{ArtifactNonce, NonceSource};
use crate::preflight::{CliProbe, PreflightFailure, TOKEN_ROTATION_RUNBOOK, preflight};
use crate::session_init::{InitReadError, SessionInit};
use crate::validation::EgressToken;

pub use flags::{FlagResult, HeadlessFlag};

/// The oldest CLI the profile can be pinned to: the first that reads
/// `permissions.blockReadsOutsideWorkingDirectories` where 03 §8 nests it (A-10).
pub const MIN_CLI_VERSION: &str = "2.1.257";

/// [`MIN_CLI_VERSION`] as numbers.
const MIN_CLI_PARTS: [u64; 3] = [2, 1, 257];

/// The file the fence canary asks the model to read, as 03 §8 writes it.
pub const FENCE_TARGET: &str = "//proc/self/environ";

/// The structured output the profile canary asks for: the three words it must repeat.
pub const PROFILE_SCHEMA: &str = r#"{"type":"object","properties":{"prompt_word":{"type":"string"},"system_word":{"type":"string"},"tree_word":{"type":"string"}},"required":["prompt_word","system_word","tree_word"],"additionalProperties":false}"#;

/// The words the profile canary plants, each a fixed prefix and a fresh nonce.
///
/// One is in the stdin prompt, one in the appended system prompt file, one in a file of the
/// `--add-dir` tree and one in a file outside the working directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileWords {
    prompt: String,
    system: String,
    tree: String,
    outside: String,
}

impl ProfileWords {
    /// The words for the nonces of the prompt, the system prompt, the tree file and the outside
    /// file, in that order.
    #[must_use]
    pub fn new([prompt, system, tree, outside]: [&ArtifactNonce; 4]) -> Self {
        Self {
            prompt: format!("ASSAYPROMPT{prompt}"),
            system: format!("ASSAYSYSTEM{system}"),
            tree: format!("ASSAYTREE{tree}"),
            outside: format!("ASSAYOUTSIDE{outside}"),
        }
    }

    /// The word the stdin prompt gives.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// The word the appended system prompt file gives.
    #[must_use]
    pub fn system(&self) -> &str {
        &self.system
    }

    /// The word of the file in the `--add-dir` tree.
    #[must_use]
    pub fn tree(&self) -> &str {
        &self.tree
    }

    /// The word of the file outside the working directories.
    #[must_use]
    pub fn outside(&self) -> &str {
        &self.outside
    }
}

/// One entry of the `result` message's `permission_denials`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDenial {
    /// The tool that was denied.
    pub tool_name: String,
    /// Its `tool_input.file_path`, when that is a string.
    pub file_path: Option<String>,
}

/// What the profile canary call showed. Built by the [`ProfileProbe`] adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRun {
    /// The session's `system/init`, or why it could not be read.
    pub init: Result<SessionInit, InitReadError>,
    /// How the call ended.
    pub outcome: ModelOutcome,
    /// The whole stream-json transcript, as capped by the runner.
    pub transcript: Vec<u8>,
    /// The init's `permissionMode`, when it is a string.
    pub permission_mode: Option<String>,
    /// The last `result` message's `permission_denials`; `None` when there is no result or the
    /// field is missing or not a list of denials.
    pub denials: Option<Vec<PermissionDenial>>,
    /// The hook messages in the stream (each named by its `hook_event`, else its `subtype`).
    pub hook_events: Vec<String>,
    /// The scopes a canary hook was planted at.
    pub planted_hooks: Vec<String>,
    /// The scopes whose canary hook wrote its marker.
    pub fired_hooks: Vec<String>,
    /// The absolute path of the planted file in the `--add-dir` tree.
    pub tree_file: String,
    /// The absolute path of the planted file outside the working directories.
    pub outside_file: String,
}

/// Runs the profile canary (03 §8). Implemented by `ClaudeCanary` in `assay-runner`.
///
/// It plants a canary hook at user, project and in-tree scope, `words`' tree and outside files
/// and an appended system prompt file holding the system word, then makes one call with the
/// production argv and `--include-hook-events`, whose stdin prompt gives the prompt word and asks
/// for [`FENCE_TARGET`], the tree file and the outside file to be read.
pub trait ProfileProbe {
    /// Why the canary could not be planted, run or inspected.
    type Error: std::error::Error;

    /// Plants, runs and inspects one profile canary.
    ///
    /// # Errors
    /// [`Self::Error`] when the checkout could not be planted, the call could not be made or
    /// supervised, or the markers could not be inspected.
    fn probe_profile(&self, words: &ProfileWords) -> Result<ProfileRun, Self::Error>;
}

/// What the profile is judged against.
#[derive(Debug, Clone, Copy)]
pub struct ProfileContext<'a> {
    /// The pinned CLI version (`claude_cli`).
    pub pinned: &'a ExactVersion,
    /// Its built-ins a clean init may list (`claude_cli_builtins`).
    pub allowlist: &'a BuiltinAllowlist,
    /// The OAuth token the call holds, which must not appear in its transcript.
    pub token: &'a EgressToken,
}

/// One profile check, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileCheck {
    /// The pin is at least [`MIN_CLI_VERSION`].
    CliMinimum,
    /// The preflight passes: no provider variable, the pinned CLI logged in by OAuth token, and
    /// a clean init of the canary call.
    Preflight,
    /// The pinned CLI answered the production argv, `--restricted` with `--safe-mode` included.
    ProfileAccepted,
    /// The init's `permissionMode` is `dontAsk`.
    PermissionMode,
    /// The reply holds the stdin prompt's word: `-p` read its prompt from stdin.
    StdinPrompt,
    /// The reply holds the appended system prompt file's word.
    SystemPromptFile,
    /// No planted hook ran and no hook event is in the stream.
    HookCanary,
    /// [`FENCE_TARGET`] was denied and the token is not in the transcript.
    FenceCanary,
    /// The `--add-dir` tree's file was read: it counts as a working directory.
    AddDir,
    /// A file outside the working directories was denied and its word never seen.
    OutsideRead,
}

impl ProfileCheck {
    /// Every check, in report order.
    pub const ALL: [Self; 10] = [
        Self::CliMinimum,
        Self::Preflight,
        Self::ProfileAccepted,
        Self::PermissionMode,
        Self::StdinPrompt,
        Self::SystemPromptFile,
        Self::HookCanary,
        Self::FenceCanary,
        Self::AddDir,
        Self::OutsideRead,
    ];
}

impl fmt::Display for ProfileCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CliMinimum => "cli-minimum",
            Self::Preflight => "preflight",
            Self::ProfileAccepted => "profile-accepted",
            Self::PermissionMode => "permission-mode",
            Self::StdinPrompt => "stdin-prompt",
            Self::SystemPromptFile => "system-prompt-file",
            Self::HookCanary => "hook-canary",
            Self::FenceCanary => "fence-canary",
            Self::AddDir => "add-dir",
            Self::OutsideRead => "outside-read",
        })
    }
}

/// A word the reply must repeat, by its field in [`PROFILE_SCHEMA`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyField {
    /// `prompt_word`: the stdin prompt's word.
    Prompt,
    /// `system_word`: the appended system prompt file's word.
    System,
    /// `tree_word`: the `--add-dir` tree file's word.
    Tree,
}

impl ReplyField {
    /// The field's key in the structured output.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Prompt => "prompt_word",
            Self::System => "system_word",
            Self::Tree => "tree_word",
        }
    }
}

impl fmt::Display for ReplyField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// Why a profile check failed; `E` is the CLI probe's error. Text the CLI or the model wrote is
/// shown escaped.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProfileFailure<E> {
    /// The pin is older than [`MIN_CLI_VERSION`].
    #[error(
        "the pinned claude_cli {pinned} is below {MIN_CLI_VERSION}, the first CLI that reads \
         permissions.blockReadsOutsideWorkingDirectories (A-10)"
    )]
    BelowMinimum {
        /// The pin.
        pinned: String,
    },
    /// The pin has a part too large to compare.
    #[error("the pinned claude_cli {pinned} cannot be compared with {MIN_CLI_VERSION}")]
    VersionUnreadable {
        /// The pin.
        pinned: String,
    },
    /// The preflight failed.
    #[error(transparent)]
    Preflight(PreflightFailure<E>),
    /// The preflight failed before the call, so the check was not run.
    #[error("not run: the preflight failed")]
    NotRun,
    /// The call's `system/init` could not be read.
    #[error("init unreadable: {0}")]
    InitUnreadable(InitReadError),
    /// The pinned CLI did not answer the production argv.
    #[error(
        "the pinned CLI did not answer the 03 §8 argv ({outcome}); if it refuses --restricted \
         with --safe-mode, using --restricted with --settings alone is an owner decision"
    )]
    ProfileRefused {
        /// How the call ended.
        outcome: &'static str,
    },
    /// The call did not answer, so what the check needs is not shown.
    #[error("the canary call did not answer ({outcome})")]
    NoAnswer {
        /// How the call ended.
        outcome: &'static str,
    },
    /// The init's `permissionMode` is not `dontAsk`.
    #[error("the init's permissionMode is {}, not dontAsk", shown(.found.as_deref()))]
    PermissionMode {
        /// What the init held; `None` when it held no string.
        found: Option<String>,
    },
    /// The reply does not hold the planted word in `field`.
    #[error("the reply's {field} is not the planted word")]
    WordMissing {
        /// The field.
        field: ReplyField,
    },
    /// No hook was planted, so the hook canary shows nothing.
    #[error("no hook was planted")]
    NothingPlanted,
    /// A planted hook wrote its marker.
    #[error("the {scope:?} canary hook ran")]
    HookRan {
        /// Its scope.
        scope: String,
    },
    /// The stream holds a hook message.
    #[error("hook event {event:?} is in the stream")]
    HookEvent {
        /// The event, or the message's subtype.
        event: String,
    },
    /// The result carries no readable `permission_denials`, so no denial is shown.
    #[error("the result carries no readable permission_denials")]
    DenialsUnreadable,
    /// No `Read` of `path` is in `permission_denials`.
    #[error("no Read of {path} is in permission_denials")]
    NotDenied {
        /// The path that should have been denied.
        path: String,
    },
    /// The transcript holds the token, or an encoding of it.
    #[error(
        "the transcript holds the OAuth token or an encoding of it; rotate the token: \
         {TOKEN_ROTATION_RUNBOOK}"
    )]
    TokenInTranscript,
    /// The transcript holds an escape the token scan cannot read back, so it may hide the token.
    #[error("the transcript holds an escape the token scan cannot read back")]
    TranscriptUnscannable,
    /// A `Read` of the tree file was denied: the `--add-dir` tree is no working directory.
    #[error("a Read of {path} in the --add-dir tree was denied")]
    TreeDenied {
        /// The tree file.
        path: String,
    },
    /// The outside file's word is in the transcript: it was read.
    #[error(
        "the outside file's word is in the transcript: a read outside the working directories was allowed"
    )]
    OutsideRead,
}

/// `found` quoted, or `missing`.
fn shown(found: Option<&str>) -> String {
    found.map_or_else(|| "missing".to_owned(), |found| format!("{found:?}"))
}

/// One check and, when it failed, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileResult<E> {
    /// The check.
    pub check: ProfileCheck,
    /// Why it failed; `None` when it passed.
    pub failure: Option<ProfileFailure<E>>,
}

/// The result of every profile check, in [`ProfileCheck::ALL`] order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileReport<E> {
    results: Vec<ProfileResult<E>>,
}

impl<E> ProfileReport<E> {
    /// Each check's result, in report order.
    #[must_use]
    pub fn results(&self) -> &[ProfileResult<E>] {
        &self.results
    }

    /// Whether every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.results.iter().all(|result| result.failure.is_none())
    }

    /// Each flag of the production argv with the checks that show it, in
    /// [`HeadlessFlag::ALL`] order.
    pub fn flags(&self) -> impl Iterator<Item = FlagResult> + '_ {
        HeadlessFlag::ALL.into_iter().map(|flag| FlagResult {
            flag,
            failed: flag
                .checks()
                .iter()
                .copied()
                .find(|&check| !self.check_passed(check)),
        })
    }

    /// Whether `check` is in the report and passed.
    fn check_passed(&self, check: ProfileCheck) -> bool {
        self.results
            .iter()
            .any(|result| result.check == check && result.failure.is_none())
    }
}

/// Why the profile could not be judged.
#[derive(Debug, Error)]
pub enum ProfileError<P, N> {
    /// No nonce could be drawn for the canary words; nothing was run.
    #[error("no canary nonce could be drawn")]
    Nonce(#[source] N),
    /// The canary could not be planted, run or inspected.
    #[error("the profile canary could not be run")]
    Probe(#[source] P),
}

/// The ports [`verify_profile`] runs through.
#[derive(Debug)]
pub struct ProfilePorts<'a, C, P, N> {
    /// Asks the pinned CLI for its version and auth status (the preflight).
    pub cli: &'a C,
    /// Runs the profile canary.
    pub probe: &'a P,
    /// Draws the canary words' nonces.
    pub nonces: &'a mut N,
}

/// What [`verify_profile`] returns for its ports: the report, or why it could not be made.
pub type ProfileVerdict<C, P, N> = Result<
    ProfileReport<<C as CliProbe>::Error>,
    ProfileError<<P as ProfileProbe>::Error, <N as NonceSource>::Error>,
>;

/// Checks the pin, runs the preflight and one profile canary, and judges them (03 §8).
///
/// The pin is held against [`MIN_CLI_VERSION`]; the preflight runs against `ports.cli` with the
/// job's `variables`; the canary runs through `ports.probe` with words from four nonces. A
/// failed preflight runs no canary: the checks after it fail as not run.
///
/// # Errors
/// [`ProfileError::Nonce`] when the nonces fail and [`ProfileError::Probe`] when the probe
/// fails. A failed check is not an error: it is in the report.
pub fn verify_profile<C: CliProbe, P: ProfileProbe, N: NonceSource>(
    variables: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ProfilePorts { cli, probe, nonces }: ProfilePorts<'_, C, P, N>,
    context: &ProfileContext<'_>,
) -> ProfileVerdict<C, P, N> {
    let minimum = judge::minimum(context.pinned);
    let proof = match preflight(variables, cli, context.pinned) {
        Ok(proof) => proof,
        Err(failed) => {
            let (_, reason) = failed.into_parts();
            return Ok(not_run(minimum, reason));
        }
    };
    let words = draw_words(nonces).map_err(ProfileError::Nonce)?;
    let run = probe.probe_profile(&words).map_err(ProfileError::Probe)?;
    let checks = judge::run(&run, &words, context.token);
    let preflight = proof
        .check_first_call(run.init, context.allowlist)
        .err()
        .map(|failed| ProfileFailure::Preflight(failed.into_parts().1));
    let results = [
        (ProfileCheck::CliMinimum, minimum),
        (ProfileCheck::Preflight, preflight),
    ]
    .into_iter()
    .chain(checks)
    .map(|(check, failure)| ProfileResult { check, failure })
    .collect();
    Ok(ProfileReport { results })
}

/// Four fresh words.
fn draw_words<N: NonceSource>(nonces: &mut N) -> Result<ProfileWords, N::Error> {
    let prompt = nonces.nonce()?;
    let system = nonces.nonce()?;
    let tree = nonces.nonce()?;
    let outside = nonces.nonce()?;
    Ok(ProfileWords::new([&prompt, &system, &tree, &outside]))
}

/// The report of a preflight that failed before the call.
fn not_run<E>(minimum: Option<ProfileFailure<E>>, reason: PreflightFailure<E>) -> ProfileReport<E> {
    let mut results = vec![
        ProfileResult {
            check: ProfileCheck::CliMinimum,
            failure: minimum,
        },
        ProfileResult {
            check: ProfileCheck::Preflight,
            failure: Some(ProfileFailure::Preflight(reason)),
        },
    ];
    results.extend(
        ProfileCheck::ALL
            .into_iter()
            .filter(|check| !matches!(check, ProfileCheck::CliMinimum | ProfileCheck::Preflight))
            .map(|check| ProfileResult {
                check,
                failure: Some(ProfileFailure::NotRun),
            }),
    );
    ProfileReport { results }
}
