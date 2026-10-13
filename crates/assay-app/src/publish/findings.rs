//! Inline comments and per-gate checks (04 §6), written by the run whose decision the verdict
//! check carries, under the per-head lock (R3-09).
//!
//! Each published finding is one review comment, at most [`MAX_INLINE_COMMENTS`] per run,
//! most severe first and then by confidence; its body names its fingerprint in a hidden marker
//! on its first line, and a fingerprint already named by one of the App's review comments on
//! the PR is never posted again, so a re-run posts no duplicate. Every free-text field of a
//! finding is model text and is made inert by the sanitiser before it is posted (#175).

mod comment;
mod gates;
mod select;

#[cfg(test)]
mod fixture;

use std::collections::BTreeSet;

use assay_domain::{Fingerprint, GateReport, Side};
use thiserror::Error;

use self::gates::GateCheck;
use super::ports::{
    CheckName, CheckStatus, CommentForge, InlineComment, PublishLedger, VerdictForge,
};
use super::sanitise::Repository;
use super::{Target, Unlock};
use crate::config::{CommitId, Sha256Digest};
use crate::validation::ValidFinding;

/// The most findings published inline per run (04 §4 step 7).
pub const MAX_INLINE_COMMENTS: usize = 8;

/// The most App review comments on one PR that are read for fingerprint markers; more refuses
/// the publication, posting nothing (H-13).
pub const MAX_PRIOR_COMMENTS: usize = 1024;

/// What [`publish_findings`] publishes for one run.
#[derive(Debug, Clone, Copy)]
pub struct FindingsPublication<'a> {
    /// The run and the PR; the PR's `repo` names the repository permalinks point into.
    pub target: Target<'a>,
    /// The decision key `publish` wrote to the verdict check: the comments are this
    /// decision's, and the gate checks carry it as their `external_id`.
    pub decision_key: &'a str,
    /// Every gate of the plan, one non-required check each.
    pub gates: &'a [GateReport],
    /// The findings Z4 validated, with the severity the verifier left them at.
    pub findings: &'a [ValidFinding],
}

/// What [`publish_findings`] did.
#[derive(Debug)]
pub enum FindingsPublished<L> {
    /// Nothing: the verdict check is missing or carries another decision (R3-09).
    Yielded {
        /// What became of the lock.
        unlock: Unlock<L>,
    },
    /// The gate checks and the comments not yet on the PR were written.
    Written(FindingsWritten<L>),
}

/// What was written.
#[derive(Debug)]
pub struct FindingsWritten<L> {
    /// The fingerprints posted now, in publication order.
    pub posted: Vec<Fingerprint>,
    /// The selected fingerprints the App had already posted on the PR, in publication order.
    pub already_posted: Vec<Fingerprint>,
    /// What became of the lock.
    pub unlock: Unlock<L>,
}

/// The forge call that failed while the lock was held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommentStep {
    /// Reading the verdict check.
    ReadCheck,
    /// Listing the App's review comments.
    ReadComments,
    /// Reading a gate check.
    ReadGateCheck,
    /// Creating or updating a gate check.
    WriteGateCheck,
    /// Posting an inline comment.
    PostComment,
}

impl CommentStep {
    const fn describe(self) -> &'static str {
        match self {
            Self::ReadCheck => "read the verdict check",
            Self::ReadComments => "list the review comments",
            Self::ReadGateCheck => "read a gate check",
            Self::WriteGateCheck => "write a gate check",
            Self::PostComment => "post an inline comment",
        }
    }
}

/// Why [`publish_findings`] could not publish.
#[derive(Debug, Error)]
pub enum FindingsError<F, L> {
    /// The PR's repository is not `owner/name` as GitHub spells them, so no permalink into it
    /// can be built safely.
    #[error("the repository name is not owner/name")]
    Repository,
    /// The decision key is not `sha256:<64 hex>`.
    #[error("the decision key is not a sha256 digest")]
    DecisionKey,
    /// A gate's name is not a plugin name.
    #[error("gate name {name:?} is not a plugin name")]
    GateName {
        /// The name as the inputs gave it.
        name: String,
    },
    /// The lock could not be taken.
    #[error("could not take the per-head publish lock")]
    Lock(#[source] L),
    /// Another writer held the lock past the adapter's wait.
    #[error("the per-head publish lock is held by another writer")]
    LockBusy,
    /// A forge call failed while the lock was held; the calls before it took effect.
    #[error("could not {} under the lock", step.describe())]
    Locked {
        /// The call.
        step: CommentStep,
        /// The forge's error.
        #[source]
        source: F,
        /// What became of the lock.
        unlock: Unlock<L>,
    },
    /// The App has more review comments on the PR than are read: nothing was written.
    #[error("the PR has {count} App review comments, more than {MAX_PRIOR_COMMENTS}")]
    TooManyComments {
        /// How many the forge returned.
        count: usize,
        /// What became of the lock.
        unlock: Unlock<L>,
    },
}

/// What [`publish_findings`] returns over its ports' error types.
pub type FindingsOutcome<F, L> = Result<
    FindingsPublished<<L as PublishLedger>::Error>,
    FindingsError<<F as VerdictForge>::Error, <L as PublishLedger>::Error>,
>;

/// Publishes `publication`'s gate checks and inline comments through `forge`, under the
/// per-head lock taken from `ledger`, after `publish` wrote the verdict.
///
/// Its inputs are checked and every body rendered before any port call. Under the lock the
/// verdict check is read: unless it carries `decision_key` the run yields, writing nothing.
/// Then the App's review comments are read (refused past [`MAX_PRIOR_COMMENTS`]), each gate's
/// check is created, or updated unless it is already completed under this key, and each
/// selected finding whose fingerprint no App comment names is posted. The lock is released
/// after every outcome that took it.
///
/// # Errors
///
/// [`FindingsError`] when the inputs are malformed (before any port call), the lock cannot be
/// taken, there are too many prior comments, or a forge call fails under the lock.
pub fn publish_findings<F: CommentForge, L: PublishLedger>(
    publication: &FindingsPublication<'_>,
    forge: &F,
    ledger: &L,
) -> FindingsOutcome<F, L> {
    let target = &publication.target;
    let event = target.pull_request;
    let repository = Repository::parse(&event.repo).ok_or(FindingsError::Repository)?;
    if publication.decision_key.parse::<Sha256Digest>().is_err() {
        return Err(FindingsError::DecisionKey);
    }
    let gate_checks = gates::gate_checks(publication.gates, publication.decision_key)
        .map_err(|name| FindingsError::GateName { name })?;
    let comments: Vec<(Fingerprint, InlineComment<'_>)> = select::select(publication.findings)
        .into_iter()
        .map(|found| {
            let comment = inline(found, repository, &event.head_sha);
            (found.fingerprint, comment)
        })
        .collect();
    let Some(lease) = ledger
        .acquire(target.run_uid.repo_id, &event.head_sha)
        .map_err(FindingsError::Lock)?
    else {
        return Err(FindingsError::LockBusy);
    };
    let locked = under_lock(publication, forge, &gate_checks, &comments);
    let unlock = match ledger.release(lease) {
        Ok(()) => Unlock::Released,
        Err(error) => Unlock::Failed(error),
    };
    match locked {
        Ok(Locked::Stale) => Ok(FindingsPublished::Yielded { unlock }),
        Ok(Locked::Written {
            posted,
            already_posted,
        }) => Ok(FindingsPublished::Written(FindingsWritten {
            posted,
            already_posted,
            unlock,
        })),
        Err(Refused::Forge(step, source)) => Err(FindingsError::Locked {
            step,
            source,
            unlock,
        }),
        Err(Refused::TooManyComments(count)) => {
            Err(FindingsError::TooManyComments { count, unlock })
        }
    }
}

/// The inline comment for `found` at `head`: a span over several lines starts at its first.
fn inline<'a>(
    found: &'a ValidFinding,
    repository: Repository<'_>,
    head: &'a CommitId,
) -> InlineComment<'a> {
    let finding = &found.finding;
    let (line, start_line) = match finding.end_line {
        Some(end) if end > finding.line => (end, Some(finding.line)),
        _ => (finding.line, None),
    };
    InlineComment {
        commit_id: head,
        path: &found.file_at_head,
        line,
        start_line,
        side: finding.side.unwrap_or(Side::Right),
        body: comment::body(found, repository, head),
    }
}

/// What happened under the lock.
enum Locked {
    Stale,
    Written {
        posted: Vec<Fingerprint>,
        already_posted: Vec<Fingerprint>,
    },
}

/// Why nothing more was written under the lock.
enum Refused<F> {
    Forge(CommentStep, F),
    TooManyComments(usize),
}

/// The calls under the lock, in order: verdict check, prior comments, gate checks, comments.
fn under_lock<F: CommentForge>(
    publication: &FindingsPublication<'_>,
    forge: &F,
    gate_checks: &[GateCheck],
    comments: &[(Fingerprint, InlineComment<'_>)],
) -> Result<Locked, Refused<F::Error>> {
    let target = &publication.target;
    let event = target.pull_request;
    let key = publication.decision_key;
    let verdict = CheckName::for_base(&event.base_ref, target.default_branch);
    let current = forge
        .find_check(&event.head_sha, &verdict)
        .map_err(|error| Refused::Forge(CommentStep::ReadCheck, error))?;
    if current.is_none_or(|check| check.external_id != key) {
        return Ok(Locked::Stale);
    }
    let prior = forge
        .app_review_comments(event.number)
        .map_err(|error| Refused::Forge(CommentStep::ReadComments, error))?;
    if prior.len() > MAX_PRIOR_COMMENTS {
        return Err(Refused::TooManyComments(prior.len()));
    }
    let mut on_pr: BTreeSet<Fingerprint> = prior
        .iter()
        .filter_map(|body| comment::posted_fingerprint(body))
        .collect();
    for check in gate_checks {
        write_gate_check(forge, &event.head_sha, check, key)?;
    }
    let mut posted = Vec::new();
    let mut already_posted = Vec::new();
    for (fp, comment) in comments {
        if !on_pr.insert(*fp) {
            already_posted.push(*fp);
            continue;
        }
        forge
            .create_review_comment(event.number, comment)
            .map_err(|error| Refused::Forge(CommentStep::PostComment, error))?;
        posted.push(*fp);
    }
    Ok(Locked::Written {
        posted,
        already_posted,
    })
}

/// Creates `check`, or updates it unless it is already completed under `key`.
fn write_gate_check<F: CommentForge>(
    forge: &F,
    head: &CommitId,
    check: &GateCheck,
    key: &str,
) -> Result<(), Refused<F::Error>> {
    let existing = forge
        .find_check(head, &check.name)
        .map_err(|error| Refused::Forge(CommentStep::ReadGateCheck, error))?;
    let written = match existing {
        Some(found) if found.external_id == key && found.status == CheckStatus::Completed => {
            return Ok(());
        }
        Some(found) => forge.update_check(found.id, &check.update),
        None => forge.create_check(head, &check.name, &check.update),
    };
    written.map_err(|error| Refused::Forge(CommentStep::WriteGateCheck, error))
}
