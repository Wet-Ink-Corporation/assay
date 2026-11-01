//! What started a re-decision (J1): the payload `GITHUB_EVENT_PATH` names, checked against
//! what guard wrote, and the reply channel it opens (D10: only a command comment is answered).

use std::num::NonZeroU64;

use assay_app::{
    CommandReplies, CommentAction, DecisionTrigger, Parsed, Reply, UserId, parse_command,
};
use assay_forge::github::{CommentPayload, DecideOnlyEvent, HeadRepository, SenderKind};

use super::{PayloadMismatch, RedecideJob, RedecideRunError};
use crate::outputs::DecideTrigger;

/// What started this re-decision, once the payload agrees with guard.
#[derive(Debug, Clone, Copy)]
pub(super) enum Trigger<'a> {
    /// A person's `/assay` command line on a new comment: answered when not acted on (D10).
    Comment {
        payload: &'a CommentPayload,
        actor: UserId,
    },
    /// An `assay:*` label change by a person.
    Label { actor: UserId },
    /// The hourly poll.
    Schedule,
}

impl Trigger<'_> {
    /// The bundle's `trigger`.
    pub(super) const fn decision(self) -> DecisionTrigger {
        match self {
            Self::Comment { payload, actor } => DecisionTrigger::Comment {
                comment_id: payload.event.comment.id,
                actor_id: actor,
            },
            Self::Label { actor } => DecisionTrigger::Label { actor_id: actor },
            Self::Schedule => DecisionTrigger::Schedule,
        }
    }

    /// Answers the triggering comment with `reply`; nothing for a label or the poll.
    pub(super) fn answer<A: CommandReplies>(
        self,
        app: &A,
        pr: NonZeroU64,
        reply: Reply,
    ) -> Result<(), A::Error> {
        match self {
            Self::Comment { payload, .. } => app.reply(pr, payload.event.comment.id, reply),
            Self::Label { .. } | Self::Schedule => Ok(()),
        }
    }
}

/// The trigger `job.event` is, checked against guard's outputs; `None` for an event no person
/// sent, or a comment that is no new `/assay` line (defence in depth: guard skipped them).
///
/// # Errors
///
/// [`RedecideRunError::Payload`] when the payload is another repository's, another event than
/// guard's `trigger`, about another PR than `job.pr`, or a label event on a head not here.
pub(super) fn trigger<'a, G, T, B, L>(
    job: &RedecideJob<'a>,
) -> Result<Option<Trigger<'a>>, RedecideRunError<G, T, B, L>> {
    let refused = |mismatch| Err(RedecideRunError::Payload(mismatch));
    let (repository, kind, pr) = match job.event {
        DecideOnlyEvent::Comment(payload) => {
            (payload.repository_id, DecideTrigger::Comment, payload.pr)
        }
        DecideOnlyEvent::Label(payload) => (
            payload.repository_id,
            DecideTrigger::Label,
            Some(payload.number),
        ),
        DecideOnlyEvent::Schedule { repository_id } => {
            (*repository_id, DecideTrigger::Schedule, Some(job.pr))
        }
    };
    if repository != job.run.run_env().run_uid.repo_id {
        return refused(PayloadMismatch::Repository);
    }
    if kind != job.guard.trigger() {
        return refused(PayloadMismatch::Trigger);
    }
    if pr != Some(job.pr) {
        return refused(PayloadMismatch::PullRequest);
    }
    Ok(match job.event {
        DecideOnlyEvent::Comment(payload) => {
            let person = payload.sender.kind != SenderKind::Bot;
            let created = payload.event.action == CommentAction::Created;
            let comment = &payload.event.comment;
            let command = parse_command(&comment.body) != Parsed::NotACommand;
            comment
                .user
                .filter(|_| person && created && command)
                .map(|actor| Trigger::Comment { payload, actor })
        }
        DecideOnlyEvent::Label(payload) => {
            if payload.head_repository != HeadRepository::Base {
                return refused(PayloadMismatch::Head);
            }
            (payload.sender.kind != SenderKind::Bot).then_some(Trigger::Label {
                actor: payload.sender.id,
            })
        }
        DecideOnlyEvent::Schedule { .. } => Some(Trigger::Schedule),
    })
}
