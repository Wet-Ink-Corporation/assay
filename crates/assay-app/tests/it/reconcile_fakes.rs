//! Fakes for the reconcile ports (TST-09): the ledger refs' lock listing over the in-memory ref
//! store, refs whose deletions fail, and one consumer repository's GitHub as the per-repo
//! reconcile reads and writes it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::num::NonZeroU64;

use assay_app::{
    CheckRunId, CheckStatus, CheckUpdate, CommitId, CompareAndSetError, LeaseRef, LedgerRefs,
    LedgerTime, LockListing, OpenPull, PullHead, ReconcileCheck, ReconcileForge, RefCreation,
    RefDeletion, RefTip, RunProgress, StampedRun,
};

use crate::lease_fakes::{MemoryRefs, RefsDown};

impl LockListing for MemoryRefs {
    fn publish_locks(&self, limit: usize) -> Result<Vec<String>, RefsDown> {
        Ok(self
            .names()
            .into_iter()
            .filter(|name| name.starts_with("refs/heads/lock-"))
            .take(limit)
            .collect())
    }
}

/// `MemoryRefs` whose every deletion fails: a lock taken through it is never released.
pub(crate) struct NoDelete<'a>(pub(crate) &'a MemoryRefs);

impl LedgerRefs for NoDelete<'_> {
    type Error = RefsDown;

    fn create(&self, name: &LeaseRef, message: &str) -> Result<RefCreation, RefsDown> {
        self.0.create(name, message)
    }

    fn read(&self, name: &LeaseRef) -> Result<Option<RefTip>, RefsDown> {
        self.0.read(name)
    }

    fn delete_if(&self, _name: &LeaseRef, _expected: &CommitId) -> Result<RefDeletion, RefsDown> {
        Err(RefsDown)
    }
}

/// GitHub could not be reached (scripted).
#[derive(Debug, thiserror::Error)]
#[error("the fake GitHub is down")]
pub(crate) struct GitHubDown;

/// One write the use case made to a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckWrite {
    pub(crate) id: CheckRunId,
    pub(crate) expected: String,
    pub(crate) update: CheckUpdate,
}

/// A check as the fake holds it.
#[derive(Debug, Clone)]
struct Held {
    head: CommitId,
    check: ReconcileCheck,
}

/// What the fake runs just before it writes a check.
type Probe = Box<dyn Fn() -> bool>;

/// One consumer repository: its open PRs, the App's checks by head, its workflow runs by id and
/// each PR's labels.
#[derive(Default)]
pub(crate) struct FakeRepo {
    pulls: Vec<OpenPull>,
    checks: RefCell<Vec<Held>>,
    runs: BTreeMap<NonZeroU64, StampedRun>,
    labels: BTreeMap<NonZeroU64, Vec<String>>,
    heads_now: BTreeMap<NonZeroU64, PullHead>,
    writes: RefCell<Vec<CheckWrite>>,
    listed: RefCell<Vec<CommitId>>,
    runs_read: RefCell<Vec<NonZeroU64>>,
    restamp_before_write: RefCell<Option<String>>,
    probe: Option<Probe>,
    probed: RefCell<Vec<bool>>,
    down: bool,
    writes_fail: bool,
}

impl FakeRepo {
    /// PR `number` is open on `head`.
    pub(crate) fn pull(mut self, number: NonZeroU64, head: CommitId) -> Self {
        self.pulls.push(OpenPull {
            number,
            head_sha: head,
        });
        self
    }

    /// The App's check `check` is on `head`.
    pub(crate) fn check(self, head: CommitId, check: ReconcileCheck) -> Self {
        self.checks.borrow_mut().push(Held { head, check });
        self
    }

    /// Workflow run `run_id` is `progress`, started at `started_at`.
    pub(crate) fn run(
        mut self,
        run_id: u64,
        progress: RunProgress,
        started_at: LedgerTime,
    ) -> Self {
        let run_id = NonZeroU64::new(run_id).expect("run ids in tests are positive");
        self.runs.insert(
            run_id,
            StampedRun {
                progress,
                started_at,
            },
        );
        self
    }

    /// PR `number` carries `label`.
    pub(crate) fn label(mut self, number: NonZeroU64, label: &str) -> Self {
        self.labels
            .entry(number)
            .or_default()
            .push(label.to_owned());
        self
    }

    /// When re-read under the lock, PR `number` is `now`, not its listed head.
    pub(crate) fn head_moves(mut self, number: NonZeroU64, now: PullHead) -> Self {
        self.heads_now.insert(number, now);
        self
    }

    /// Just before our first check write, another writer re-stamps the check `external_id`.
    pub(crate) fn restamped_to(self, external_id: &str) -> Self {
        *self.restamp_before_write.borrow_mut() = Some(external_id.to_owned());
        self
    }

    /// Just before each check write, `probe` runs; [`Self::probed`] holds what it returned.
    pub(crate) fn probing(mut self, probe: impl Fn() -> bool + 'static) -> Self {
        self.probe = Some(Box::new(probe));
        self
    }

    /// Every call fails.
    pub(crate) fn down(mut self) -> Self {
        self.down = true;
        self
    }

    /// Every check write fails; reads succeed.
    pub(crate) fn writes_fail(mut self) -> Self {
        self.writes_fail = true;
        self
    }

    /// Every check write made, in order.
    pub(crate) fn writes(&self) -> Vec<CheckWrite> {
        self.writes.borrow().clone()
    }

    /// The heads whose checks were listed, in order.
    pub(crate) fn listed(&self) -> Vec<CommitId> {
        self.listed.borrow().clone()
    }

    /// The workflow runs read, in order.
    pub(crate) fn runs_read(&self) -> Vec<NonZeroU64> {
        self.runs_read.borrow().clone()
    }

    /// What the probe returned before each check write.
    pub(crate) fn probed(&self) -> Vec<bool> {
        self.probed.borrow().clone()
    }

    fn up(&self) -> Result<(), GitHubDown> {
        if self.down { Err(GitHubDown) } else { Ok(()) }
    }
}

impl ReconcileForge for FakeRepo {
    type Error = GitHubDown;

    fn open_pulls(&self, limit: usize) -> Result<Vec<OpenPull>, GitHubDown> {
        self.up()?;
        Ok(self.pulls.iter().take(limit).cloned().collect())
    }

    fn app_checks(&self, head: &CommitId, limit: usize) -> Result<Vec<ReconcileCheck>, GitHubDown> {
        self.up()?;
        self.listed.borrow_mut().push(head.clone());
        Ok(self
            .checks
            .borrow()
            .iter()
            .filter(|held| held.head == *head)
            .take(limit)
            .map(|held| held.check.clone())
            .collect())
    }

    fn workflow_run(&self, run_id: NonZeroU64) -> Result<Option<StampedRun>, GitHubDown> {
        self.up()?;
        self.runs_read.borrow_mut().push(run_id);
        Ok(self.runs.get(&run_id).copied())
    }

    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, GitHubDown> {
        self.up()?;
        Ok(self.labels.get(&number).cloned().unwrap_or_default())
    }

    fn pull_head(&self, number: NonZeroU64) -> Result<PullHead, GitHubDown> {
        self.up()?;
        if let Some(now) = self.heads_now.get(&number) {
            return Ok(now.clone());
        }
        Ok(self
            .pulls
            .iter()
            .find(|pull| pull.number == number)
            .map_or(PullHead::Closed, |pull| {
                PullHead::Open(pull.head_sha.clone())
            }))
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        update: &CheckUpdate,
    ) -> Result<(), CompareAndSetError<GitHubDown>> {
        self.up().map_err(CompareAndSetError::Forge)?;
        if let Some(probe) = &self.probe {
            self.probed.borrow_mut().push(probe());
        }
        if self.writes_fail {
            return Err(CompareAndSetError::Forge(GitHubDown));
        }
        let mut checks = self.checks.borrow_mut();
        let held = checks
            .iter_mut()
            .find(|held| held.check.id == id)
            .ok_or(CompareAndSetError::Forge(GitHubDown))?;
        if let Some(restamp) = self.restamp_before_write.borrow_mut().take() {
            held.check.external_id = Some(restamp);
        }
        let found = held.check.external_id.clone().unwrap_or_default();
        if found != expected {
            return Err(CompareAndSetError::Conflict { found });
        }
        held.check.external_id = Some(update.external_id.clone());
        held.check.status = CheckStatus::Completed;
        self.writes.borrow_mut().push(CheckWrite {
            id,
            expected: expected.to_owned(),
            update: update.clone(),
        });
        Ok(())
    }
}

/// The head numbered `n`, as 40 hex digits.
pub(crate) fn head(n: u64) -> CommitId {
    format!("{n:040x}")
        .parse()
        .expect("40 hex digits are a commit id")
}

/// An App check `id` named `name`, in progress, stamped `external_id`.
pub(crate) fn check(id: u64, name: &str, external_id: &str) -> ReconcileCheck {
    ReconcileCheck {
        id: CheckRunId(NonZeroU64::new(id).expect("check ids in tests are positive")),
        name: name.to_owned(),
        external_id: Some(external_id.to_owned()),
        status: CheckStatus::InProgress,
    }
}
