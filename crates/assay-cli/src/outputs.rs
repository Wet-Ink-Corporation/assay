//! Each pipeline job's outputs as typed values (03 §4a, D10 A).
//!
//! The job that writes them encodes them and the jobs that read them decode them from
//! `ASSAY_NEEDS`, one pair per job, so producer and consumer cannot drift.

mod admit;
mod decide;
mod decide_only;
mod gates;
mod plan;
mod review;

use std::num::NonZeroU64;

use assay_app::{CommitId, UserId};

pub use self::admit::{AdmitOutputs, HeldSlot};
pub use self::decide::{DECIDE_NEEDS, DecideNeeds, DecideOutputs, PlanEnd, admit_job};
pub use self::decide_only::{
    DECIDE_ONLY_SKIP, DecideOnlyGuardOutputs, DecideOnlyPrs, DecideTrigger, MAX_DECIDE_ONLY_PRS,
    PrsError,
};
pub use self::gates::{GatesOutputError, gates_from_needs, gates_output};
pub use self::plan::PlanOutputs;
pub use self::review::{REVIEW_PLACEHOLDER, ReviewEnd, ReviewReport};
use crate::ci::{
    JobName, JobResult, Needs, NeedsError, OutputName, Upstream, canonical_positive, shown,
};

/// The run identity `guard` resolves from the event (I2): every later job reads it from
/// `guard`'s outputs, never from the event payload.
///
/// The fields are public and `base_ref` and `default_branch` are plain strings, so the
/// producer must pass branch names it has already validated: non-empty, with no control
/// character, as `assay_forge::github::EventPayload::read` returns them.
/// [`Identity::from_needs`] refuses any other branch name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The PR number.
    pub pr: NonZeroU64,
    /// The head commit the run is pinned to.
    pub head_sha: CommitId,
    /// The base commit the run is pinned to.
    pub base_sha: CommitId,
    /// The base branch.
    pub base_ref: String,
    /// Whether the PR is a draft.
    pub draft: bool,
    /// The PR's author.
    pub author: UserId,
    /// The event's sender.
    pub sender: UserId,
    /// The event behind the run.
    pub trigger: Trigger,
    /// The repository's default branch.
    pub default_branch: String,
}

/// The event behind the run, as `guard` reports it. A `synchronize` push's `forced` is not
/// here: `guard` cannot know it, and `open` determines it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// `pull_request_target` `opened`.
    Opened,
    /// `pull_request_target` `synchronize`, with the head before the push.
    Synchronize {
        /// The head before the push.
        before: CommitId,
    },
    /// `pull_request_target` `reopened`.
    Reopened,
    /// `pull_request_target` `ready_for_review`.
    ReadyForReview,
    /// `pull_request_target` `edited` with a base change.
    Edited,
    /// `workflow_dispatch`.
    Dispatch,
}

impl Trigger {
    /// The `trigger` output's value.
    const fn word(&self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Synchronize { .. } => "synchronize",
            Self::Reopened => "reopened",
            Self::ReadyForReview => "ready_for_review",
            Self::Edited => "edited",
            Self::Dispatch => "workflow_dispatch",
        }
    }
}

/// The job whose outputs [`Identity`] is.
const GUARD: JobName = JobName::Guard;

impl Identity {
    /// `guard`'s outputs for this identity, in [`OutputName::GUARD`]'s order: `run=true`
    /// (an identity exists only for an event the pipeline runs for), then each field;
    /// `before` only for [`Trigger::Synchronize`].
    ///
    /// The branch names are written as given, so they must already be valid (non-empty, no
    /// control character; see [`Identity`]). [`Outputs::write`](crate::Outputs::write) still
    /// refuses a line break, and [`Identity::from_needs`] refuses any other invalid name.
    #[must_use]
    pub fn outputs(&self) -> Vec<(OutputName, String)> {
        let flag = |value: bool| flag_word(value).to_owned();
        let mut outputs = vec![
            (OutputName::Run, flag(true)),
            (OutputName::Pr, self.pr.to_string()),
            (OutputName::HeadSha, self.head_sha.to_string()),
            (OutputName::BaseSha, self.base_sha.to_string()),
            (OutputName::BaseRef, self.base_ref.clone()),
            (OutputName::Draft, flag(self.draft)),
            (OutputName::AuthorId, self.author.0.to_string()),
            (OutputName::SenderId, self.sender.0.to_string()),
            (OutputName::Trigger, self.trigger.word().to_owned()),
        ];
        if let Trigger::Synchronize { before } = &self.trigger {
            outputs.push((OutputName::Before, before.to_string()));
        }
        outputs.push((OutputName::DefaultBranch, self.default_branch.clone()));
        outputs
    }

    /// The identity `guard` wrote, read from `needs`. An empty output reads as not written
    /// (GitHub's value for an unset output).
    ///
    /// # Errors
    ///
    /// [`NeedsError::MissingJob`] without `guard`; [`NeedsError::NotSucceeded`] when it did
    /// not succeed; [`NeedsError::UnexpectedOutput`] for an output that is not guard's;
    /// [`NeedsError::MissingOutput`] for a required one absent or empty, and
    /// [`NeedsError::Output`] for one that does not parse, a `run` other than `true`, or a
    /// `before` beside a trigger other than `synchronize`.
    pub fn from_needs(needs: &Needs) -> Result<Self, NeedsError> {
        let read = Read::succeeded(needs, GUARD, &OutputName::GUARD)?;
        let invalid = |name| read.invalid(name);
        if read.required(OutputName::Run)? != "true" {
            return Err(invalid(OutputName::Run));
        }
        let trigger = match read.required(OutputName::Trigger)? {
            "synchronize" => Trigger::Synchronize {
                before: read.parsed(OutputName::Before, |text| text.parse().ok())?,
            },
            word => {
                let trigger = match word {
                    "opened" => Trigger::Opened,
                    "reopened" => Trigger::Reopened,
                    "ready_for_review" => Trigger::ReadyForReview,
                    "edited" => Trigger::Edited,
                    "workflow_dispatch" => Trigger::Dispatch,
                    _ => return Err(invalid(OutputName::Trigger)),
                };
                if read.optional(OutputName::Before).is_some() {
                    return Err(invalid(OutputName::Before));
                }
                trigger
            }
        };
        Ok(Self {
            pr: read.parsed(OutputName::Pr, canonical_positive)?,
            head_sha: read.parsed(OutputName::HeadSha, |text| text.parse().ok())?,
            base_sha: read.parsed(OutputName::BaseSha, |text| text.parse().ok())?,
            base_ref: read.parsed(OutputName::BaseRef, branch)?,
            draft: read.parsed(OutputName::Draft, parse_flag)?,
            author: read.parsed(OutputName::AuthorId, user)?,
            sender: read.parsed(OutputName::SenderId, user)?,
            trigger,
            default_branch: read.parsed(OutputName::DefaultBranch, branch)?,
        })
    }
}

/// Reads one upstream job's outputs by name.
struct Read<'n> {
    job: JobName,
    upstream: &'n Upstream,
}

impl<'n> Read<'n> {
    /// The outputs of `job` in `needs`, which must have succeeded and written only `names`.
    ///
    /// # Errors
    ///
    /// [`NeedsError::MissingJob`] without `job`; [`NeedsError::NotSucceeded`] when it did
    /// not succeed; [`NeedsError::UnexpectedOutput`] for an output not in `names`.
    fn succeeded(needs: &'n Needs, job: JobName, names: &[OutputName]) -> Result<Self, NeedsError> {
        let upstream = needs.job(job).ok_or(NeedsError::MissingJob { job })?;
        if upstream.result() != JobResult::Success {
            return Err(NeedsError::NotSucceeded {
                job,
                result: upstream.result(),
            });
        }
        Self::own(job, upstream, names)
    }

    /// The outputs of `job` in `needs`, whatever it ended with, which must be only `names`.
    ///
    /// # Errors
    ///
    /// [`NeedsError::MissingJob`] without `job`; [`NeedsError::UnexpectedOutput`] for an
    /// output not in `names`.
    fn any_result(
        needs: &'n Needs,
        job: JobName,
        names: &[OutputName],
    ) -> Result<Self, NeedsError> {
        let upstream = needs.job(job).ok_or(NeedsError::MissingJob { job })?;
        Self::own(job, upstream, names)
    }

    /// `upstream`'s outputs as `job`'s, refusing the first that is not in `names`.
    fn own(job: JobName, upstream: &'n Upstream, names: &[OutputName]) -> Result<Self, NeedsError> {
        if let Some((name, _)) = upstream
            .outputs()
            .find(|&(name, _)| !names.iter().any(|own| own.as_str() == name))
        {
            return Err(NeedsError::UnexpectedOutput {
                job,
                name: shown(name),
            });
        }
        Ok(Self { job, upstream })
    }

    /// Output `name`, unless absent or empty.
    fn optional(&self, name: OutputName) -> Option<&'n str> {
        self.upstream
            .output(name.as_str())
            .filter(|value| !value.is_empty())
    }

    /// Output `name`, which must be written.
    fn required(&self, name: OutputName) -> Result<&'n str, NeedsError> {
        self.optional(name).ok_or(NeedsError::MissingOutput {
            job: self.job,
            name,
        })
    }

    /// Output `name`, which must be written and parse with `parse`.
    fn parsed<T>(
        &self,
        name: OutputName,
        parse: impl FnOnce(&str) -> Option<T>,
    ) -> Result<T, NeedsError> {
        parse(self.required(name)?).ok_or_else(|| self.invalid(name))
    }

    /// The refusal of output `name`'s value.
    const fn invalid(&self, name: OutputName) -> NeedsError {
        NeedsError::Output {
            job: self.job,
            name,
        }
    }
}

/// A flag output's value, `true` or `false`.
const fn flag_word(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// A flag output's value read back: exactly `true` or `false`.
fn parse_flag(text: &str) -> Option<bool> {
    match text {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// `text` as a branch name: not empty, no control character.
fn branch(text: &str) -> Option<String> {
    (!text.chars().any(char::is_control)).then(|| text.to_owned())
}

/// `text` as a user id.
fn user(text: &str) -> Option<UserId> {
    canonical_positive(text).map(UserId)
}

#[cfg(test)]
mod tests;
