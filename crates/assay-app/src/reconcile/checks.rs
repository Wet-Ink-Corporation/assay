//! The per-repo reconcile (06 §4, 02 §5, R3-06, R3-16): one consumer repository closes its own
//! verdict checks that no run will finish. It never reaps a slot lease or writes a cost record;
//! that is the ledger-side reconcile's alone.

use std::collections::HashSet;
use std::iter;
use std::num::NonZeroU64;
use std::time::Duration;

use assay_domain::RunUid;

use super::outcome::{
    ChecksReconcileError, ChecksReconciled, ClosedCheck, Closure, Listing, LostLock, STUCK_MINUTES,
    Skip, SkippedCheck, closure_update,
};
use super::ports::{OpenPull, PullHead, ReconcileCheck, ReconcileForge, RunProgress};
use crate::config::CommitId;
use crate::forge::{CompareAndSetError, Deadline, LockedWrite};
use crate::ledger::{
    Ledger, LedgerRefs, LedgerTime, LockAcquired, LockRequest, PushKind, Released, acquire_lock,
    read_pushes, read_state, release_lock,
};
use crate::publish::CheckStatus;

/// How long after the run that opened a verdict check started the check may stay in progress
/// before it is stuck: 2 × the `full` tier's p95.
///
/// That p95 is the 25-minute critical path of 06 §3. An in-progress check does not say its
/// tier, so every check is given the slowest tier's time.
pub const STUCK_AFTER: Duration = Duration::from_mins(STUCK_MINUTES);

/// The most open PRs one sweep reads; more are refused, never cut.
pub const MAX_OPEN_PULLS: usize = 256;

/// The most App check runs read on one head; more are refused.
pub const MAX_HEAD_CHECKS: usize = 256;

/// How many of a PR's earlier heads, latest first from its ledger push records, are swept for
/// superseded checks. A run on an older head was cancelled by a push long since swept.
pub const MAX_SUPERSEDED_HEADS: usize = 16;

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
/// (publish-error)` when the run its `pending:<run_id>:<seq>` stamp names (the last run that
/// opened it, on whatever commit: a requeue or decide-only run is on the default branch's) has
/// completed and started more than [`STUCK_AFTER`] ago, and no requeue is still scheduled: the
/// PR carries [`LLM_PENDING_LABEL`] *and* its ledger deferral record is for this head with fewer
/// than `max_requeues` requeues (the label alone never excludes a check, R3-15). A check with
/// any other stamp, or whose run GitHub no longer has, is left and reported. A verdict check not
/// completed on one of the PR's [`MAX_SUPERSEDED_HEADS`] latest earlier heads (from its ledger
/// push records; the scan stops at the first head with none), when no open PR is on that head,
/// is closed `cancelled`.
///
/// The sweep reads everything and checks every cap before it writes anything. Each write holds
/// the head's publish lock (02 §5), re-reads the PR's head, and is conditioned on the
/// `external_id` read, so a run that re-stamped the check meanwhile wins.
///
/// # Errors
///
/// [`ChecksReconcileError::NotFirstAttempt`] for a re-run, before any port is called; a listing
/// past its cap, or a failed read, before any write; otherwise the first failed write or lock.
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
    // Read first, write after: a cap or failed read refuses the whole sweep, never part of it.
    let mut due = Vec::new();
    for pull in &pulls {
        sweep.stuck(pull, &mut due, &mut done.skipped)?;
        sweep.superseded(pull, &mut due)?;
    }
    for each in due {
        sweep.close(each, &mut done)?;
    }
    Ok(done)
}

/// One sweep's ports, request and the heads of the open PRs.
struct Sweep<'a, F, L, R> {
    ports: ReconcilePorts<'a, F, L, R>,
    request: &'a ChecksReconcileRequest<'a>,
    current: HashSet<&'a CommitId>,
}

/// What the stamped run says of an in-progress verdict check.
enum Judged {
    /// Its run completed more than [`STUCK_AFTER`] after it started.
    Stuck,
    /// Its run is live, or started less than [`STUCK_AFTER`] ago.
    NotStuck,
    /// Nothing says how old it is.
    Unjudged(Skip),
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

    /// Adds `pull`'s stuck verdict checks to `due`, and those it cannot judge to `skipped`.
    fn stuck(
        &self,
        pull: &OpenPull,
        due: &mut Vec<Due>,
        skipped: &mut Vec<SkippedCheck>,
    ) -> Result<(), Failed<F, L, R>> {
        // Each reported or due check owns its head: they are reported one by one.
        let mut old = Vec::new();
        for check in self.open_verdicts(&pull.head_sha)? {
            match self.judge(&check)? {
                Judged::Stuck => old.push(check),
                Judged::NotStuck => {}
                Judged::Unjudged(reason) => skipped.push(SkippedCheck {
                    pr: pull.number,
                    head: pull.head_sha.clone(),
                    check: check.id,
                    reason,
                }),
            }
        }
        if old.is_empty() || self.requeue_scheduled(pull)? {
            return Ok(());
        }
        due.extend(old.into_iter().map(|check| Due {
            pr: pull.number,
            head: pull.head_sha.clone(),
            check,
            closure: Closure::Stuck,
        }));
        Ok(())
    }

    /// Reads the run `check`'s stamp names and judges the check by it.
    fn judge(&self, check: &ReconcileCheck) -> Result<Judged, Failed<F, L, R>> {
        let Some(run_id) = check.external_id.as_deref().and_then(stamped_run_id) else {
            return Ok(Judged::Unjudged(Skip::NotPending));
        };
        let Some(run) = self
            .ports
            .forge
            .workflow_run(run_id)
            .map_err(ChecksReconcileError::Forge)?
        else {
            return Ok(Judged::Unjudged(Skip::RunGone));
        };
        let now = self.request.now;
        let old = run
            .started_at
            .checked_add(STUCK_AFTER)
            .is_some_and(|due| now > due);
        Ok(if old && run.progress == RunProgress::Completed {
            Judged::Stuck
        } else {
            Judged::NotStuck
        })
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

    /// Adds the open verdict checks on `pull`'s superseded heads to `due`, latest head first,
    /// down to the first head with none: a head below it was swept when it was superseded.
    fn superseded(&self, pull: &OpenPull, due: &mut Vec<Due>) -> Result<(), Failed<F, L, R>> {
        for head in self.superseded_heads(pull)? {
            let checks = self.open_verdicts(&head)?;
            if checks.is_empty() {
                break;
            }
            due.extend(checks.into_iter().map(|check| Due {
                pr: pull.number,
                // Each due check owns its head: they are reported one by one.
                head: head.clone(),
                check,
                closure: Closure::Superseded,
            }));
        }
        Ok(())
    }

    /// `pull`'s latest earlier heads from its ledger push records, latest first, none of them
    /// the head of an open PR.
    fn superseded_heads(&self, pull: &OpenPull) -> Result<Vec<CommitId>, Failed<F, L, R>> {
        let pushes = read_pushes(self.ports.ledger, self.request.run_uid.repo_id, pull.number)
            .map_err(ChecksReconcileError::Ledger)?;
        let mut heads: Vec<CommitId> = Vec::with_capacity(MAX_SUPERSEDED_HEADS);
        // A push names the head it moved from too; an opener has no earlier head.
        let seen = pushes.into_iter().rev().flat_map(|record| {
            let before = match record.push.kind {
                PushKind::Push { before, .. } => Some(before),
                PushKind::Opener => None,
            };
            iter::once(record.push.after).chain(before)
        });
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
    fn close(&self, due: Due, done: &mut ChecksReconciled) -> Result<(), Failed<F, L, R>> {
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
            // The request owns its head; `due` still reports it.
            head: due.head.clone(),
        };
        let LockAcquired::Held(hold) = acquire_lock(self.ports.refs, &lock, self.request.now)
            .map_err(ChecksReconcileError::Lock)?
        else {
            done.skipped.push(due.skipped(Skip::LockBusy));
            return Ok(());
        };
        let written = self.write(&due, expected, hold.deadline());
        let released = release_lock(self.ports.refs, &hold);
        let skip = match (written, released) {
            (Err(source), released) => {
                return Err(ChecksReconcileError::Write {
                    source,
                    release: released.err(),
                });
            }
            (Ok(_), Err(release)) => return Err(ChecksReconcileError::Lock(release)),
            (Ok(skip), Ok(released)) => {
                if skip.is_none() && released == Released::NotHeld {
                    done.lost_locks.push(due.lost_lock());
                }
                skip
            }
        };
        match skip {
            None => done.closed.push(due.closed()),
            Some(reason) => done.skipped.push(due.skipped(reason)),
        }
        Ok(())
    }

    /// Under the lock: re-reads the PR's head and writes `due`'s closure to its check if the
    /// head still makes it that closure and the check still carries `expected`. `None` when
    /// written, else why not. The write sends and waits for nothing past `deadline`, the lock
    /// lease's expiry (#290).
    fn write(
        &self,
        due: &Due,
        expected: &str,
        deadline: Deadline,
    ) -> Result<Option<Skip>, F::Error> {
        let now = self.ports.forge.pull_head(due.pr)?;
        let on_head = matches!(&now, PullHead::Open(current) if *current == due.head);
        let still = match due.closure {
            Closure::Stuck => on_head,
            Closure::Superseded => !on_head,
        };
        if !still {
            return Ok(Some(Skip::HeadMoved));
        }
        let update = closure_update(due.closure, self.request.run_uid);
        let write = LockedWrite {
            update: &update,
            deadline,
        };
        match self
            .ports
            .forge
            .update_check_if(due.check.id, expected, write)
        {
            Ok(()) => Ok(None),
            Err(CompareAndSetError::Conflict { .. }) => Ok(Some(Skip::Moved)),
            Err(CompareAndSetError::Forge(error)) => Err(error),
        }
    }
}

/// A check due to be closed: on which PR and head, and how.
struct Due {
    pr: NonZeroU64,
    head: CommitId,
    check: ReconcileCheck,
    closure: Closure,
}

impl Due {
    fn closed(self) -> ClosedCheck {
        ClosedCheck {
            pr: self.pr,
            head: self.head,
            check: self.check.id,
            closure: self.closure,
        }
    }

    fn skipped(self, reason: Skip) -> SkippedCheck {
        SkippedCheck {
            pr: self.pr,
            head: self.head,
            check: self.check.id,
            reason,
        }
    }

    fn lost_lock(&self) -> LostLock {
        LostLock {
            pr: self.pr,
            // Reported twice: the lock lost here, and the check closed in `closed`.
            head: self.head.clone(),
            check: self.check.id,
        }
    }
}

/// The run id of `open`'s stamp `pending:<run_id>:<seq>`, both ASCII digits and the id
/// positive; `None` for any other text.
fn stamped_run_id(stamp: &str) -> Option<NonZeroU64> {
    let (run_id, seq) = stamp.strip_prefix("pending:")?.split_once(':')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(run_id) || !digits(seq) {
        return None;
    }
    run_id.parse().ok()
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
