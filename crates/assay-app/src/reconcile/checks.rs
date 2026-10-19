//! The per-repo reconcile (06 §4, 02 §5, R3-06, R3-16): one consumer repository closes its own
//! verdict checks that no run will finish. It never reaps a slot lease or writes a cost record;
//! that is the ledger-side reconcile's alone.

use std::collections::HashSet;
use std::num::NonZeroU64;
use std::time::Duration;

use assay_domain::RunUid;

use super::outcome::{
    ChecksReconcileError, ChecksReconciled, ClosedCheck, Closure, Listing, STUCK_MINUTES, Skip,
    SkippedCheck, closure_update,
};
use super::ports::{OpenPull, PullHead, ReconcileCheck, ReconcileForge};
use crate::config::CommitId;
use crate::forge::CompareAndSetError;
use crate::ledger::{
    Ledger, LedgerRefs, LedgerTime, LockAcquired, LockRequest, acquire_lock, read_pushes,
    read_state, release_lock,
};
use crate::publish::CheckStatus;

/// How long a verdict check may stay in progress before it is stuck: 2 × the `full` tier's p95.
///
/// That p95 is the 25-minute critical path of 06 §3. An in-progress check does not say its
/// tier, so every check is given the slowest tier's time.
pub const STUCK_AFTER: Duration = Duration::from_mins(STUCK_MINUTES);

/// The most open PRs one sweep reads; more are refused, never cut.
pub const MAX_OPEN_PULLS: usize = 256;

/// The most App check runs read on one head; more are refused.
pub const MAX_HEAD_CHECKS: usize = 256;

/// The most live workflow runs read on one head; more are refused.
pub const MAX_HEAD_RUNS: usize = 100;

/// How many of a PR's earlier heads, latest first from its ledger push records, are swept for
/// superseded checks. A run on an older head was cancelled by a push long since swept.
pub const MAX_SUPERSEDED_HEADS: usize = 16;

/// The workflows whose live run may still publish a verdict: the pipeline and decide-only
/// (02 §5's provenance rules name them).
pub const ASSAY_WORKFLOWS: [&str; 2] = [
    ".github/workflows/assay.yml",
    ".github/workflows/assay-decide.yml",
];

/// The label a deferred run is projected as (06 §1).
pub const LLM_PENDING_LABEL: &str = "assay:llm-pending";

/// The verdict check's name on the default branch; on any other base it is followed by ` @<base>`.
const VERDICT: &str = "assay / verdict";

/// What the per-repo reconcile is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecksReconcileRequest<'a> {
    /// The reconcile's own run, in the consumer repository (`repo_id` names its locks); only its
    /// first attempt runs (R3-05).
    pub run_uid: RunUid,
    /// The consumer repository, `owner/name`.
    pub repo: &'a str,
    /// The instant ages are judged at.
    pub now: LedgerTime,
    /// The rubric's requeue limit: a deferral with fewer requeues still has one scheduled.
    pub max_requeues: u32,
}

/// The ports the per-repo reconcile uses: GitHub, the ledger's files (push records and PR state,
/// read only) and its refs (the per-head publish lock only).
#[derive(Debug, Clone, Copy)]
pub struct ReconcilePorts<'a, F, L, R> {
    /// The consumer repository.
    pub forge: &'a F,
    /// The ledger repository's files.
    pub ledger: &'a L,
    /// The ledger repository's refs.
    pub refs: &'a R,
}

type Failed<F, L, R> = ChecksReconcileError<
    <F as ReconcileForge>::Error,
    <L as Ledger>::Error,
    <R as LedgerRefs>::Error,
>;

/// Sweeps the consumer repository's verdict checks at `request.now`.
///
/// For each open PR, a verdict check still in progress on its head is closed `failure
/// (publish-error)` when it started more than [`STUCK_AFTER`] ago, no run of an
/// [`ASSAY_WORKFLOWS`] workflow is live on the head, and no requeue is still scheduled: the PR
/// carries [`LLM_PENDING_LABEL`] *and* its ledger deferral record is for this head with fewer
/// than `max_requeues` requeues (the label alone never excludes a check, R3-15). A verdict check
/// not completed on one of the PR's [`MAX_SUPERSEDED_HEADS`] latest earlier heads (from its
/// ledger push records), when no open PR is on that head, is closed `cancelled`.
///
/// Each write holds the head's publish lock (02 §5), re-reads the PR's head, and is
/// conditioned on the `external_id` read, so a run that re-stamped the check meanwhile wins.
///
/// # Errors
///
/// [`ChecksReconcileError::NotFirstAttempt`] for a re-run, before any port is called; otherwise
/// the first port failure or a listing past its cap.
pub fn reconcile_checks<F: ReconcileForge, L: Ledger, R: LedgerRefs>(
    ports: ReconcilePorts<'_, F, L, R>,
    request: &ChecksReconcileRequest<'_>,
) -> Result<ChecksReconciled, Failed<F, L, R>> {
    let attempt = request.run_uid.run_attempt;
    if attempt.get() != 1 {
        return Err(ChecksReconcileError::NotFirstAttempt { attempt });
    }
    let pulls = capped(
        ports.forge.open_pulls(MAX_OPEN_PULLS.saturating_add(1)),
        Listing::OpenPulls,
        MAX_OPEN_PULLS,
    )?;
    let current: HashSet<&CommitId> = pulls.iter().map(|pull| &pull.head_sha).collect();
    let sweep = Sweep {
        ports,
        request,
        current,
    };
    let mut done = ChecksReconciled::default();
    for pull in &pulls {
        for check in &sweep.stuck(pull)? {
            let due = Due {
                pr: pull.number,
                head: &pull.head_sha,
                check,
                closure: Closure::Stuck,
            };
            sweep.close(due, &mut done)?;
        }
        for head in &sweep.superseded_heads(pull)? {
            for check in &sweep.open_verdicts(head)? {
                let due = Due {
                    pr: pull.number,
                    head,
                    check,
                    closure: Closure::Superseded,
                };
                sweep.close(due, &mut done)?;
            }
        }
    }
    Ok(done)
}

/// One sweep's ports, request and the heads of the open PRs.
struct Sweep<'a, F, L, R> {
    ports: ReconcilePorts<'a, F, L, R>,
    request: &'a ChecksReconcileRequest<'a>,
    current: HashSet<&'a CommitId>,
}

impl<F: ReconcileForge, L: Ledger, R: LedgerRefs> Sweep<'_, F, L, R> {
    /// The verdict checks on `head` that have not completed.
    fn open_verdicts(&self, head: &CommitId) -> Result<Vec<ReconcileCheck>, Failed<F, L, R>> {
        let checks = capped(
            self.ports
                .forge
                .app_checks(head, MAX_HEAD_CHECKS.saturating_add(1)),
            Listing::Checks,
            MAX_HEAD_CHECKS,
        )?;
        Ok(checks
            .into_iter()
            .filter(|check| check.status != CheckStatus::Completed && is_verdict(&check.name))
            .collect())
    }

    /// The verdict checks on `pull`'s head that are stuck.
    fn stuck(&self, pull: &OpenPull) -> Result<Vec<ReconcileCheck>, Failed<F, L, R>> {
        let now = self.request.now;
        let old: Vec<ReconcileCheck> = self
            .open_verdicts(&pull.head_sha)?
            .into_iter()
            .filter(|check| {
                check
                    .started_at
                    .checked_add(STUCK_AFTER)
                    .is_some_and(|due| now > due)
            })
            .collect();
        if old.is_empty() || self.live(&pull.head_sha)? || self.requeue_scheduled(pull)? {
            return Ok(Vec::new());
        }
        Ok(old)
    }

    /// Whether a run of an assay workflow on `head` has not completed.
    fn live(&self, head: &CommitId) -> Result<bool, Failed<F, L, R>> {
        let runs = capped(
            self.ports
                .forge
                .live_runs(head, MAX_HEAD_RUNS.saturating_add(1)),
            Listing::Runs,
            MAX_HEAD_RUNS,
        )?;
        Ok(runs
            .iter()
            .any(|run| ASSAY_WORKFLOWS.contains(&run.path.as_str())))
    }

    /// Whether `pull` is deferred with a requeue still scheduled: labelled, and its ledger
    /// deferral record is for its head with requeues left (the ledger decides, R3-15).
    fn requeue_scheduled(&self, pull: &OpenPull) -> Result<bool, Failed<F, L, R>> {
        let labels = self
            .ports
            .forge
            .labels(pull.number)
            .map_err(ChecksReconcileError::Forge)?;
        if !labels.iter().any(|label| label == LLM_PENDING_LABEL) {
            return Ok(false);
        }
        let state = read_state(self.ports.ledger, self.request.run_uid.repo_id, pull.number)
            .map_err(ChecksReconcileError::Ledger)?;
        Ok(state.deferral.is_some_and(|deferral| {
            deferral.head == pull.head_sha && deferral.count < self.request.max_requeues
        }))
    }

    /// `pull`'s latest earlier heads from its ledger push records, latest first, none of them
    /// the head of an open PR.
    fn superseded_heads(&self, pull: &OpenPull) -> Result<Vec<CommitId>, Failed<F, L, R>> {
        let pushes = read_pushes(self.ports.ledger, self.request.run_uid.repo_id, pull.number)
            .map_err(ChecksReconcileError::Ledger)?;
        let mut heads: Vec<CommitId> = Vec::with_capacity(MAX_SUPERSEDED_HEADS);
        let seen = pushes
            .into_iter()
            .rev()
            .flat_map(|record| [record.push.after, record.push.before]);
        for head in seen {
            if heads.len() == MAX_SUPERSEDED_HEADS {
                break;
            }
            if !self.current.contains(&head) && !heads.contains(&head) {
                heads.push(head);
            }
        }
        Ok(heads)
    }

    /// Closes the check `due` names under its head's publish lock, recording what happened in
    /// `done`.
    fn close(&self, due: Due<'_>, done: &mut ChecksReconciled) -> Result<(), Failed<F, L, R>> {
        let Some(expected) = due
            .check
            .external_id
            .as_deref()
            .filter(|stamp| !stamp.is_empty())
        else {
            done.skipped.push(due.skipped(Skip::NoStamp));
            return Ok(());
        };
        let lock = LockRequest {
            run_uid: self.request.run_uid,
            repo: self.request.repo.to_owned(),
            head: due.head.clone(),
        };
        let LockAcquired::Held(hold) = acquire_lock(self.ports.refs, &lock, self.request.now)
            .map_err(ChecksReconcileError::Lock)?
        else {
            done.skipped.push(due.skipped(Skip::LockBusy));
            return Ok(());
        };
        let written = self.write(due, expected);
        // When the write failed, its failure is the one reported: a lock this release could not
        // delete expires with its 2-minute TTL and is reaped (F21).
        let released = release_lock(self.ports.refs, &hold).map_err(ChecksReconcileError::Lock);
        match written? {
            None => done.closed.push(due.closed()),
            Some(reason) => done.skipped.push(due.skipped(reason)),
        }
        released.map(drop)
    }

    /// Under the lock: re-reads the PR's head and writes `due`'s closure to its check if the
    /// head still makes it that closure and the check still carries `expected`. `None` when
    /// written, else why not.
    fn write(&self, due: Due<'_>, expected: &str) -> Result<Option<Skip>, Failed<F, L, R>> {
        let now = self
            .ports
            .forge
            .pull_head(due.pr)
            .map_err(ChecksReconcileError::Forge)?;
        let on_head = matches!(&now, PullHead::Open(current) if current == due.head);
        let still = match due.closure {
            Closure::Stuck => on_head,
            Closure::Superseded => !on_head,
        };
        if !still {
            return Ok(Some(Skip::HeadMoved));
        }
        let update = closure_update(due.closure, self.request.run_uid);
        match self
            .ports
            .forge
            .update_check_if(due.check.id, expected, &update)
        {
            Ok(()) => Ok(None),
            Err(CompareAndSetError::Conflict { .. }) => Ok(Some(Skip::Moved)),
            Err(CompareAndSetError::Forge(error)) => Err(ChecksReconcileError::Forge(error)),
        }
    }
}

/// A check due to be closed: on which PR and head, and how.
#[derive(Clone, Copy)]
struct Due<'a> {
    pr: NonZeroU64,
    head: &'a CommitId,
    check: &'a ReconcileCheck,
    closure: Closure,
}

impl Due<'_> {
    fn closed(self) -> ClosedCheck {
        ClosedCheck {
            pr: self.pr,
            head: self.head.clone(),
            check: self.check.id,
            closure: self.closure,
        }
    }

    fn skipped(self, reason: Skip) -> SkippedCheck {
        SkippedCheck {
            pr: self.pr,
            head: self.head.clone(),
            check: self.check.id,
            reason,
        }
    }
}

/// Whether `name` is a verdict check's: `assay / verdict`, or `assay / verdict @<base>`.
fn is_verdict(name: &str) -> bool {
    name.strip_prefix(VERDICT).is_some_and(|rest| {
        rest.is_empty() || rest.strip_prefix(" @").is_some_and(|base| !base.is_empty())
    })
}

/// The listing `listed`, refused when it holds more than `limit` items.
fn capped<T, F, L, R>(
    listed: Result<Vec<T>, F>,
    what: Listing,
    limit: usize,
) -> Result<Vec<T>, ChecksReconcileError<F, L, R>> {
    let listed = listed.map_err(ChecksReconcileError::Forge)?;
    if listed.len() > limit {
        return Err(ChecksReconcileError::TooMany { what, limit });
    }
    Ok(listed)
}
