//! The judgements of the profile canary's run (03 §8, #84), each failing closed.
//!
//! Every judge borrows the run, whose init is moved into the preflight's second phase last, and
//! a failure owns what it shows, so the few names a failure quotes are copied out (H-08).

use serde_json::Value;

use super::{
    FENCE_TARGET, MIN_CLI_PARTS, PermissionDenial, ProfileCheck, ProfileFailure, ProfileRun,
    ProfileWords, ReplyField,
};
use crate::config::ExactVersion;
use crate::doctor::contains_ignoring_case;
use crate::model_runner::ModelOutcome;
use crate::validation::{EgressKind, EgressScan, EgressToken, KnownRuns};

/// The permission mode of the 03 §8 profile.
const DONT_ASK: &str = "dontAsk";

/// The tool every canary read uses.
const READ: &str = "Read";

/// The pin against [`MIN_CLI_VERSION`](super::MIN_CLI_VERSION).
pub(super) fn minimum<E>(pinned: &ExactVersion) -> Option<ProfileFailure<E>> {
    match version_parts(pinned.as_str()) {
        None => Some(ProfileFailure::VersionUnreadable {
            pinned: pinned.as_str().to_owned(),
        }),
        Some(parts) if parts < MIN_CLI_PARTS => Some(ProfileFailure::BelowMinimum {
            pinned: pinned.as_str().to_owned(),
        }),
        Some(_) => None,
    }
}

/// `<major>.<minor>.<patch>` as numbers; `None` when a part does not fit a `u64`.
fn version_parts(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.').map(str::parse::<u64>);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => Some([major, minor, patch]),
        _ => None,
    }
}

/// Every check the call alone decides, in report order after the preflight.
pub(super) fn run<E>(
    run: &ProfileRun,
    words: &ProfileWords,
    token: &EgressToken,
) -> [(ProfileCheck, Option<ProfileFailure<E>>); 8] {
    [
        (ProfileCheck::ProfileAccepted, accepted(run)),
        (ProfileCheck::PermissionMode, permission_mode(run)),
        (
            ProfileCheck::StdinPrompt,
            reply_word(&run.outcome, ReplyField::Prompt, words.prompt()),
        ),
        (
            ProfileCheck::SystemPromptFile,
            reply_word(&run.outcome, ReplyField::System, words.system()),
        ),
        (ProfileCheck::HookCanary, hooks(run)),
        (ProfileCheck::FenceCanary, fence(run, token)),
        (ProfileCheck::AddDir, add_dir(run, words.tree())),
        (ProfileCheck::OutsideRead, outside(run, words.outside())),
    ]
}

/// The production argv was accepted: the call answered and its init was read. A call that did
/// not answer is reported first, since a CLI that refuses a flag writes no stream at all.
fn accepted<E>(run: &ProfileRun) -> Option<ProfileFailure<E>> {
    match (&run.outcome, &run.init) {
        (ModelOutcome::Ok(_), Ok(_)) => None,
        // The report owns its failures; the run keeps its init for the preflight.
        (ModelOutcome::Ok(_), Err(error)) => Some(ProfileFailure::InitUnreadable(error.clone())),
        (outcome, _) => Some(ProfileFailure::ProfileRefused {
            outcome: outcome_name(outcome),
        }),
    }
}

fn permission_mode<E>(run: &ProfileRun) -> Option<ProfileFailure<E>> {
    (run.permission_mode.as_deref() != Some(DONT_ASK)).then(|| ProfileFailure::PermissionMode {
        // The report owns what it shows; the run is borrowed by the other checks.
        found: run.permission_mode.clone(),
    })
}

/// The reply's `field` is `word`, trimmed and in any ASCII case.
fn reply_word<E>(
    outcome: &ModelOutcome,
    field: ReplyField,
    word: &str,
) -> Option<ProfileFailure<E>> {
    let ModelOutcome::Ok(structured) = outcome else {
        return Some(no_answer(outcome));
    };
    let said = structured
        .as_value()
        .get(field.key())
        .and_then(Value::as_str);
    if said.is_some_and(|said| said.trim().eq_ignore_ascii_case(word)) {
        None
    } else {
        Some(ProfileFailure::WordMissing { field })
    }
}

/// The hook canary: something was planted, no marker was written, no hook message was streamed,
/// and the call answered (so the session ran its course: its prompt, tool calls and stop).
fn hooks<E>(run: &ProfileRun) -> Option<ProfileFailure<E>> {
    if run.planted_hooks.is_empty() {
        Some(ProfileFailure::NothingPlanted)
    } else if let Some(scope) = run.fired_hooks.first() {
        Some(ProfileFailure::HookRan {
            scope: scope.clone(),
        })
    } else if let Some(event) = run.hook_events.first() {
        Some(ProfileFailure::HookEvent {
            event: event.clone(),
        })
    } else if let ModelOutcome::Ok(_) = run.outcome {
        None
    } else {
        Some(no_answer(&run.outcome))
    }
}

/// The fence canary: the token (in any encoding the egress scan reads) is nowhere in the
/// transcript, and the read of [`FENCE_TARGET`] was denied.
fn fence<E>(run: &ProfileRun, token: &EgressToken) -> Option<ProfileFailure<E>> {
    match EgressScan::with_token(token, KnownRuns::default()).scan_rejected(&run.transcript) {
        Some(EgressKind::Token) => Some(ProfileFailure::TokenInTranscript),
        Some(_) => Some(ProfileFailure::TranscriptUnscannable),
        None => denied(run, FENCE_TARGET),
    }
}

/// The `--add-dir` tree's file was not denied and its word came back.
fn add_dir<E>(run: &ProfileRun, word: &str) -> Option<ProfileFailure<E>> {
    let tree_denied = run
        .denials
        .iter()
        .flatten()
        .any(|denial| is_read_of(denial, &run.tree_file));
    if tree_denied {
        Some(ProfileFailure::TreeDenied {
            path: run.tree_file.clone(),
        })
    } else {
        reply_word(&run.outcome, ReplyField::Tree, word)
    }
}

/// The outside file's word is nowhere in the transcript, and its read was denied.
fn outside<E>(run: &ProfileRun, word: &str) -> Option<ProfileFailure<E>> {
    if contains_ignoring_case(&run.transcript, word.as_bytes()) {
        Some(ProfileFailure::OutsideRead)
    } else {
        denied(run, &run.outside_file)
    }
}

/// A `Read` of `path` is among the run's denials.
fn denied<E>(run: &ProfileRun, path: &str) -> Option<ProfileFailure<E>> {
    match &run.denials {
        None => Some(ProfileFailure::DenialsUnreadable),
        Some(denials) if denials.iter().any(|denial| is_read_of(denial, path)) => None,
        Some(_) => Some(ProfileFailure::NotDenied {
            path: path.to_owned(),
        }),
    }
}

/// `denial` is a `Read` of the absolute `path`, however many slashes lead either.
fn is_read_of(denial: &PermissionDenial, path: &str) -> bool {
    denial.tool_name == READ
        && denial.file_path.as_deref().is_some_and(|found| {
            found.starts_with('/') && found.trim_start_matches('/') == path.trim_start_matches('/')
        })
}

fn no_answer<E>(outcome: &ModelOutcome) -> ProfileFailure<E> {
    ProfileFailure::NoAnswer {
        outcome: outcome_name(outcome),
    }
}

/// How a call ended, as the report names it.
const fn outcome_name(outcome: &ModelOutcome) -> &'static str {
    match outcome {
        ModelOutcome::Ok(_) => "answered",
        ModelOutcome::BudgetExhausted => "budget-exhausted",
        ModelOutcome::TurnsExhausted => "turns-exhausted",
        ModelOutcome::Timeout => "timeout",
        ModelOutcome::QuotaExhausted => "quota-exhausted",
        ModelOutcome::Malformed => "malformed",
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_CLI_PARTS, version_parts};
    use crate::doctor::profile::MIN_CLI_VERSION;

    #[test]
    fn the_minimum_parts_are_the_minimum_version() {
        assert_eq!(version_parts(MIN_CLI_VERSION), Some(MIN_CLI_PARTS));
    }
}
