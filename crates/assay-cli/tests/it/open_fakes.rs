//! Fakes for `run_open` (TST-09): the reads open makes before it writes, a GitHub holding check
//! runs, an in-memory ledger and lease-ref store, and a clock fixed at one instant.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use assay_app::{
    CheckFound, CheckName, CheckRun, CheckRunId, CheckState, CheckStatus, CommitId, CommitIdentity,
    CompareAndSetError, FileVersion, Forge, LeaseRef, Ledger, LedgerFile, LedgerPath, LedgerRefs,
    LedgerTime, LockedWrite, OpenClock, Reaction, ReactionSubject, RefCreation, RefDeletion,
    RefTip, Review, TimelineEvent, Upserted, WriteCondition, WriteOutcome,
};
use assay_cli::OpenReads;

/// A fake refused the call (scripted, or a call open never makes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) &'static str);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the fake refused {}", self.0)
    }
}

impl std::error::Error for Refused {}

/// One read open made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Read {
    CreatedAt(NonZeroU64),
    Forced { before: CommitId, after: CommitId },
}

/// The run's `created_at` and the compare's answer, recording each read.
pub(crate) struct FakeReads {
    created_at: Result<LedgerTime, Refused>,
    forced: Result<bool, Refused>,
    reads: RefCell<Vec<Read>>,
}

impl FakeReads {
    /// The run was created at `created_at`, and a push is `forced` or not.
    pub(crate) fn new(created_at: LedgerTime, forced: bool) -> Self {
        Self {
            created_at: Ok(created_at),
            forced: Ok(forced),
            reads: RefCell::new(Vec::new()),
        }
    }

    /// The run cannot be read.
    pub(crate) fn run_unreadable(self) -> Self {
        Self {
            created_at: Err(Refused("run_created_at")),
            ..self
        }
    }

    /// The comparison cannot be read.
    pub(crate) fn compare_unreadable(self) -> Self {
        Self {
            forced: Err(Refused("forced")),
            ..self
        }
    }

    /// Every read made, in order.
    pub(crate) fn reads(&self) -> Vec<Read> {
        self.reads.borrow().clone()
    }
}

impl OpenReads for FakeReads {
    type Error = Refused;

    fn run_created_at(&self, run_id: NonZeroU64) -> Result<LedgerTime, Refused> {
        self.reads.borrow_mut().push(Read::CreatedAt(run_id));
        self.created_at
    }

    fn forced(&self, before: &CommitId, after: &CommitId) -> Result<bool, Refused> {
        self.reads.borrow_mut().push(Read::Forced {
            before: before.clone(),
            after: after.clone(),
        });
        self.forced
    }
}

/// One check run the fake holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldCheck {
    pub(crate) head: CommitId,
    pub(crate) name: String,
    pub(crate) run: CheckRun,
    pub(crate) state: CheckState,
}

/// A GitHub that holds check runs; open makes no other call.
#[derive(Default)]
pub(crate) struct FakeGitHub {
    checks: RefCell<Vec<HeldCheck>>,
    next_id: Cell<u64>,
}

impl FakeGitHub {
    /// Every check held, in creation order.
    pub(crate) fn checks(&self) -> Vec<HeldCheck> {
        self.checks.borrow().clone()
    }
}

/// The status a written state gives a check.
const fn status_of(state: CheckState) -> CheckStatus {
    match state {
        CheckState::InProgress => CheckStatus::InProgress,
        CheckState::Completed(_) => CheckStatus::Completed,
    }
}

impl Forge for FakeGitHub {
    type Error = Refused;

    fn find_or_create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: LockedWrite<'_>,
    ) -> Result<CheckFound, Refused> {
        let mut checks = self.checks.borrow_mut();
        if let Some(found) = checks
            .iter()
            .find(|check| check.head == *head && check.name == name.as_str())
        {
            return Ok(CheckFound::Found(found.run.clone()));
        }
        self.next_id.set(self.next_id.get().saturating_add(1));
        let run = CheckRun {
            id: CheckRunId(NonZeroU64::MIN.saturating_add(self.next_id.get())),
            external_id: initial.update.external_id.clone(),
            status: status_of(initial.update.state),
        };
        checks.push(HeldCheck {
            head: head.clone(),
            name: name.as_str().to_owned(),
            run: run.clone(),
            state: initial.update.state,
        });
        Ok(CheckFound::Created(run))
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        write: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<Refused>> {
        let mut checks = self.checks.borrow_mut();
        let check = checks
            .iter_mut()
            .find(|check| check.run.id == id)
            .ok_or(CompareAndSetError::Forge(Refused("an unknown check")))?;
        if check.run.external_id != expected {
            return Err(CompareAndSetError::Conflict {
                found: check.run.external_id.clone(),
            });
        }
        check.run.external_id.clone_from(&write.update.external_id);
        check.run.status = status_of(write.update.state);
        check.state = write.update.state;
        Ok(())
    }

    fn upsert_comment(&self, _: NonZeroU64, _: &str, _: &str) -> Result<Upserted, Refused> {
        Err(Refused("upsert_comment"))
    }

    fn commit_identities(&self, _: &[CommitId]) -> Result<Vec<CommitIdentity>, Refused> {
        Err(Refused("commit_identities"))
    }

    fn reviews(&self, _: NonZeroU64) -> Result<Vec<Review>, Refused> {
        Err(Refused("reviews"))
    }

    fn labels(&self, _: NonZeroU64) -> Result<Vec<String>, Refused> {
        Err(Refused("labels"))
    }

    fn reactions(&self, _: ReactionSubject) -> Result<Vec<Reaction>, Refused> {
        Err(Refused("reactions"))
    }

    fn timeline(&self, _: NonZeroU64) -> Result<Vec<TimelineEvent>, Refused> {
        Err(Refused("timeline"))
    }
}

/// The ledger's files, each with a version that moves on every write.
#[derive(Default)]
pub(crate) struct MemoryLedger {
    files: RefCell<BTreeMap<String, (u64, Vec<u8>)>>,
}

impl MemoryLedger {
    /// Every file, by path.
    pub(crate) fn files(&self) -> BTreeMap<String, String> {
        self.files
            .borrow()
            .iter()
            .map(|(path, (_, bytes))| (path.clone(), String::from_utf8_lossy(bytes).into_owned()))
            .collect()
    }
}

impl Ledger for MemoryLedger {
    type Error = Refused;

    fn read(&self, path: &LedgerPath) -> Result<Option<LedgerFile>, Refused> {
        Ok(self
            .files
            .borrow()
            .get(path.as_str())
            .map(|(version, bytes)| LedgerFile {
                version: FileVersion::new(version.to_string()),
                contents: bytes.clone(),
            }))
    }

    fn write(
        &self,
        path: &LedgerPath,
        contents: &[u8],
        condition: &WriteCondition,
    ) -> Result<WriteOutcome, Refused> {
        let mut files = self.files.borrow_mut();
        let current = files.get(path.as_str()).map(|(version, _)| *version);
        let holds = match (condition, current) {
            (WriteCondition::Absent, None) => true,
            (WriteCondition::At(version), Some(current)) => version.as_str() == current.to_string(),
            (WriteCondition::Absent, Some(_)) | (WriteCondition::At(_), None) => false,
        };
        if !holds {
            return Ok(WriteOutcome::Conflict);
        }
        let next = current.map_or(1, |version| version.saturating_add(1));
        files.insert(path.as_str().to_owned(), (next, contents.to_vec()));
        Ok(WriteOutcome::Written)
    }
}

/// Lease refs: created only when absent, deleted only at the commit given.
#[derive(Default)]
pub(crate) struct MemoryRefs {
    refs: RefCell<BTreeMap<String, RefTip>>,
    commits: Cell<u64>,
    deleted: RefCell<Vec<String>>,
}

impl MemoryRefs {
    /// The refs held now, by name.
    pub(crate) fn names(&self) -> Vec<String> {
        self.refs.borrow().keys().cloned().collect()
    }

    /// The refs deleted, in order.
    pub(crate) fn deleted(&self) -> Vec<String> {
        self.deleted.borrow().clone()
    }
}

impl LedgerRefs for MemoryRefs {
    type Error = Refused;

    fn create(&self, name: &LeaseRef, message: &str) -> Result<RefCreation, Refused> {
        let mut refs = self.refs.borrow_mut();
        if refs.contains_key(name.as_str()) {
            return Ok(RefCreation::Exists);
        }
        self.commits.set(self.commits.get().saturating_add(1));
        let commit: CommitId = format!("{:040x}", self.commits.get())
            .parse()
            .map_err(|_not_an_id| Refused("a commit id"))?;
        refs.insert(
            name.as_str().to_owned(),
            RefTip {
                commit: commit.clone(),
                message: message.to_owned(),
            },
        );
        Ok(RefCreation::Created(commit))
    }

    fn read(&self, name: &LeaseRef) -> Result<Option<RefTip>, Refused> {
        Ok(self.refs.borrow().get(name.as_str()).cloned())
    }

    fn delete_if(&self, name: &LeaseRef, expected: &CommitId) -> Result<RefDeletion, Refused> {
        let mut refs = self.refs.borrow_mut();
        if refs
            .get(name.as_str())
            .is_some_and(|tip| tip.commit == *expected)
        {
            refs.remove(name.as_str());
            self.deleted.borrow_mut().push(name.as_str().to_owned());
            return Ok(RefDeletion::Deleted);
        }
        Ok(RefDeletion::Moved)
    }
}

/// A clock fixed at one instant; a pause is recorded, never slept (TST-10).
pub(crate) struct FixedClock {
    now: LedgerTime,
    pauses: RefCell<Vec<u8>>,
}

impl FixedClock {
    /// A clock reading `now`.
    pub(crate) const fn at(now: LedgerTime) -> Self {
        Self {
            now,
            pauses: RefCell::new(Vec::new()),
        }
    }
}

impl OpenClock for FixedClock {
    type Error = Refused;

    fn now(&self) -> Result<LedgerTime, Refused> {
        Ok(self.now)
    }

    fn pause(&self, retry: u8) {
        self.pauses.borrow_mut().push(retry);
    }
}
