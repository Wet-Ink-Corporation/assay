//! `doctor`'s session checks (`docs/architecture/03-contracts.md` §8, #121): one canary call
//! under the headless profile in a throwaway checkout, judged for a clean init, drift between
//! the observed built-ins and the pinned allowlist, a loaded or named mod (the mod canary) and a
//! followed AGENTS.md/CLAUDE.md instruction (the AGENTS.md canary).
//!
//! The token's recorded expiry is judged by [`token_expiry_warning`] (05 §5, #93).
//!
//! Every check fails closed: a check that cannot see what it needs (an unreadable init, a call
//! that did not answer, a canary that planted nothing) fails rather than passes.

mod token_expiry;

use std::fmt;

use serde_json::Value;
use thiserror::Error;

use crate::config::{BuiltinAllowlist, ExactVersion};
use crate::model_runner::ModelOutcome;
use crate::plan::{ArtifactNonce, NonceSource};
use crate::session_init::{
    AllowlistDrift, InitReadError, InitUnclean, SessionInit, allowlist_drift, check_init,
};

pub use token_expiry::{EXPIRY_WARNING_DAYS, ExpiryWarning, token_expiry_warning};

/// The fixed first part of every canary word.
const WORD_PREFIX: &str = "ASSAYCANARY";

/// The word the planted AGENTS.md and CLAUDE.md files tell the model to write.
///
/// It is [`CanaryToken::PREFIX`] followed by a fresh nonce. The files hold the two parts apart,
/// so the joined word appears in a transcript only if the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanaryToken {
    suffix: String,
}

impl CanaryToken {
    /// The word for `nonce`.
    #[must_use]
    pub fn new(nonce: &ArtifactNonce) -> Self {
        Self {
            suffix: nonce.to_string(),
        }
    }

    /// The first part of every word.
    pub const PREFIX: &'static str = WORD_PREFIX;

    /// The second part, the nonce.
    #[must_use]
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// Whether `transcript` holds the joined word, in any ASCII case.
    fn appears_in(&self, transcript: &[u8]) -> bool {
        let word = [WORD_PREFIX.as_bytes(), self.suffix.as_bytes()].concat();
        contains_ignoring_case(transcript, &word)
    }

    /// Whether a message the model wrote holds the nonce, in any ASCII case.
    ///
    /// Every JSON message counts except `user` ones, which carry the tool results that quote
    /// the planted files. A line that is not a JSON object is not a message; the joined word
    /// is still looked for in it by [`Self::appears_in`].
    fn written_by_model(&self, transcript: &[u8]) -> bool {
        transcript
            .split(|&byte| byte == b'\n')
            .filter(|line| contains_ignoring_case(line, self.suffix.as_bytes()))
            .any(is_model_message)
    }
}

/// Whether `haystack` holds `needle`, in any ASCII case. Every text holds the empty needle
/// (and `windows` refuses a zero size).
fn contains_ignoring_case(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Whether `line` is a JSON message other than a `user` one (tool results).
fn is_model_message(line: &[u8]) -> bool {
    match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(message)) => message.get("type").and_then(Value::as_str) != Some("user"),
        Ok(_) | Err(_) => false,
    }
}

/// What one canary call showed. Built by the [`CanaryProbe`] adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanaryRun {
    /// The session's `system/init`, or why it could not be read.
    pub init: Result<SessionInit, InitReadError>,
    /// How the call ended.
    pub outcome: ModelOutcome,
    /// The whole stream-json transcript, as capped by the runner.
    pub transcript: Vec<u8>,
    /// The names of the mods the probe planted.
    pub planted_mods: Vec<String>,
    /// The planted mods whose `session.start` hook wrote its marker file.
    pub loaded_mods: Vec<String>,
}

/// Runs the canary call (03 §8). Implemented by `ClaudeCanary` in `assay-runner`.
///
/// It plants a user-scope mod and skills-folder mods whose `session.start` hook writes a marker,
/// and AGENTS.md and CLAUDE.md files telling the model to write `token`'s word, in the working
/// directory and the `--add-dir` tree, then makes one call under the headless profile.
pub trait CanaryProbe {
    /// Why the canary could not be planted, run or inspected.
    type Error: std::error::Error;

    /// Plants, runs and inspects one canary.
    ///
    /// # Errors
    /// [`Self::Error`] when the checkout could not be planted, the call could not be made or
    /// supervised, or the markers could not be inspected.
    fn probe(&self, token: &CanaryToken) -> Result<CanaryRun, Self::Error>;
}

/// One check `doctor` reports, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The canary session's init is clean (03 §8).
    CleanInit,
    /// The session's built-ins are exactly the pinned allowlist, so a CLI pin bump without an
    /// allowlist update fails.
    BuiltinAllowlist,
    /// No planted mod ran or was loaded.
    ModCanary,
    /// The model did not follow the planted AGENTS.md or CLAUDE.md instruction.
    AgentsMdCanary,
}

impl Check {
    /// Every check, in report order.
    pub const ALL: [Self; 4] = [
        Self::CleanInit,
        Self::BuiltinAllowlist,
        Self::ModCanary,
        Self::AgentsMdCanary,
    ];
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CleanInit => "clean-init",
            Self::BuiltinAllowlist => "builtin-allowlist",
            Self::ModCanary => "mod-canary",
            Self::AgentsMdCanary => "agents-md-canary",
        })
    }
}

/// Why a check failed. Names are CLI output and are shown escaped.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckFailure {
    /// The session's `system/init` could not be read, so nothing that depends on it is shown.
    #[error("init unreadable: {0}")]
    InitUnreadable(InitReadError),
    /// The session's init is not clean.
    #[error("init-unclean: {0}")]
    InitUnclean(InitUnclean),
    /// The observed built-ins are not the pinned allowlist.
    #[error("the observed built-ins differ from claude_cli_builtins: {0}")]
    Drift(AllowlistDrift),
    /// A planted mod's `session.start` hook wrote its marker.
    #[error("mod {name:?} ran its session.start hook")]
    ModLoaded {
        /// The mod.
        name: String,
    },
    /// A planted mod is listed in `init.plugins`.
    #[error("mod {name:?} is listed in init.plugins")]
    ModNamed {
        /// The mod.
        name: String,
    },
    /// The probe planted no mod, so the mod canary shows nothing.
    #[error("no mod was planted")]
    NothingPlanted,
    /// The call did not answer, so whether the instruction was followed is not shown.
    #[error("the canary call did not answer")]
    NoAnswer,
    /// The transcript holds the canary word: the instruction was followed.
    #[error("the model followed the planted AGENTS.md/CLAUDE.md instruction")]
    InstructionFollowed,
    /// A message the model wrote (not a tool result) holds the canary nonce: it saw the
    /// planted instruction, whether or not it wrote the word whole.
    #[error("the model wrote the nonce of the planted AGENTS.md/CLAUDE.md instruction")]
    NonceWritten,
}

/// One check and, when it failed, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// The check.
    pub check: Check,
    /// Why it failed; `None` when it passed.
    pub failure: Option<CheckFailure>,
}

/// The result of every check, in [`Check::ALL`] order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    results: Vec<CheckResult>,
}

impl DoctorReport {
    /// Each check's result, in report order.
    #[must_use]
    pub fn results(&self) -> &[CheckResult] {
        &self.results
    }

    /// Whether every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.results.iter().all(|result| result.failure.is_none())
    }
}

/// Why `doctor` could not judge the session.
#[derive(Debug, Error)]
pub enum DoctorError<P, N> {
    /// No nonce could be drawn for the canary word; nothing was run.
    #[error("no canary nonce could be drawn")]
    Nonce(#[source] N),
    /// The canary could not be planted, run or inspected.
    #[error("the canary could not be run")]
    Probe(#[source] P),
}

/// Runs one canary through `probe` and judges it against the pinned CLI version and its
/// built-in allowlist (03 §8).
///
/// # Errors
/// [`DoctorError::Nonce`] when `nonces` fails (before the probe runs) and
/// [`DoctorError::Probe`] when the probe fails. A failed check is not an error: it is in the
/// report.
pub fn doctor<P: CanaryProbe, N: NonceSource>(
    probe: &P,
    nonces: &mut N,
    pinned: &ExactVersion,
    allowlist: &BuiltinAllowlist,
) -> Result<DoctorReport, DoctorError<P::Error, N::Error>> {
    let token = CanaryToken::new(&nonces.nonce().map_err(DoctorError::Nonce)?);
    let run = probe.probe(&token).map_err(DoctorError::Probe)?;
    let results = Check::ALL
        .into_iter()
        .map(|check| CheckResult {
            check,
            failure: judge(check, &run, &token, (pinned, allowlist)),
        })
        .collect();
    Ok(DoctorReport { results })
}

fn judge(
    check: Check,
    run: &CanaryRun,
    token: &CanaryToken,
    (pinned, allowlist): (&ExactVersion, &BuiltinAllowlist),
) -> Option<CheckFailure> {
    match check {
        Check::CleanInit => with_init(run, |init| {
            check_init(init, pinned, allowlist)
                .err()
                .map(CheckFailure::InitUnclean)
        }),
        Check::BuiltinAllowlist => with_init(run, |init| {
            allowlist_drift(init, allowlist).map(CheckFailure::Drift)
        }),
        Check::ModCanary => mods(run),
        Check::AgentsMdCanary => agents_md(run, token),
    }
}

/// `judge` applied to the run's init, or `InitUnreadable` when there is none.
fn with_init(
    run: &CanaryRun,
    judge: impl FnOnce(&SessionInit) -> Option<CheckFailure>,
) -> Option<CheckFailure> {
    match &run.init {
        Ok(init) => judge(init),
        // The report owns its failures; the run keeps its own copy of the error.
        Err(error) => Some(CheckFailure::InitUnreadable(error.clone())),
    }
}

/// The mod canary: something was planted, no marker was written and no planted mod is listed.
///
/// The markers are judged before the init, so a mod that ran is reported even when the init
/// cannot be read.
fn mods(run: &CanaryRun) -> Option<CheckFailure> {
    if run.planted_mods.is_empty() {
        return Some(CheckFailure::NothingPlanted);
    }
    if let Some(name) = run.loaded_mods.first() {
        return Some(CheckFailure::ModLoaded { name: name.clone() });
    }
    with_init(run, |init| {
        init.plugins()
            .iter()
            .find(|plugin| run.planted_mods.contains(&plugin.name))
            .map(|plugin| CheckFailure::ModNamed {
                name: plugin.name.clone(),
            })
    })
}

/// The AGENTS.md canary: the call answered and the word appears nowhere in its transcript.
fn agents_md(run: &CanaryRun, token: &CanaryToken) -> Option<CheckFailure> {
    if token.appears_in(&run.transcript) {
        Some(CheckFailure::InstructionFollowed)
    } else if token.written_by_model(&run.transcript) {
        Some(CheckFailure::NonceWritten)
    } else if matches!(run.outcome, ModelOutcome::Ok(_)) {
        None
    } else {
        Some(CheckFailure::NoAnswer)
    }
}
