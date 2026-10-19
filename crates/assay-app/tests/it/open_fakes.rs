//! Fakes for `open` (TST-09): a GitHub that holds check runs and records every call with whether
//! the publish lock was held at that moment, and a clock that replays scripted instants and
//! records its pauses.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::num::NonZeroU64;

use assay_app::{
    CheckFound, CheckName, CheckRun, CheckRunId, CheckState, CheckStatus, CheckUpdate, CommitId,
    CommitIdentity, CompareAndSetError, Forge, LedgerTime, OpenClock, Reaction, ReactionSubject,
    Review, TimelineEvent, Upserted,
};

use crate::lease_fakes::MemoryRefs;
use crate::ledger_fakes::{at, commit};

/// GitHub could not be reached (scripted).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("the fake GitHub refused the call")]
pub(crate) struct GitHubDown;

/// One check run as the fake holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldCheck {
    pub(crate) id: CheckRunId,
    pub(crate) head: CommitId,
    pub(crate) name: String,
    pub(crate) external_id: String,
    pub(crate) status: CheckStatus,
    /// The last update written, the creation's included.
    pub(crate) last: CheckUpdate,
}

/// One call `open` made, with whether the publish lock ref existed when it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForgeCall {
    /// `find_or_create_check`; `created` when it created one.
    FindOrCreate {
        name: String,
        created: bool,
        locked: bool,
    },
    /// `update_check_if`.
    Update {
        id: CheckRunId,
        expected: String,
        update: CheckUpdate,
        locked: bool,
    },
    /// Any other `Forge` call: `open` reads no PR content (R3-07, 05 §4).
    Unexpected(&'static str),
}

/// How the fake answers a write that sets a `completed` check back to `in_progress` (A-30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Reopen {
    /// It reopens, conclusion cleared.
    #[default]
    Accepts,
    /// It answers success but the check stays `completed` with its old conclusion.
    KeepsCompleted,
    /// It refuses the write.
    Rejects,
}

/// The fake GitHub, checking the lock against `refs`.
pub(crate) struct FakeGitHub<'r> {
    refs: &'r MemoryRefs,
    lock: String,
    checks: RefCell<Vec<HeldCheck>>,
    calls: RefCell<Vec<ForgeCall>>,
    next_id: Cell<u64>,
    reopen: Cell<Reopen>,
    down: Cell<bool>,
    rival_stamp: RefCell<Option<String>>,
    rival_after: RefCell<Option<String>>,
}

impl<'r> FakeGitHub<'r> {
    /// A GitHub with no check, whose writes are judged against lock ref `lock` of `refs`.
    pub(crate) fn new(refs: &'r MemoryRefs, lock: &str) -> Self {
        Self {
            refs,
            lock: lock.to_owned(),
            checks: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
            next_id: Cell::new(500),
            reopen: Cell::new(Reopen::Accepts),
            down: Cell::new(false),
            rival_stamp: RefCell::new(None),
            rival_after: RefCell::new(None),
        }
    }

    /// Holds the App's check `name` on head `bbbb…` with `external_id` and `status`; returns
    /// its id.
    pub(crate) fn holding(&self, name: &str, external_id: &str, status: CheckStatus) -> CheckRunId {
        let head = commit('b');
        let id = self.fresh_id();
        let state = match status {
            CheckStatus::Completed => CheckState::Completed(assay_app::CheckConclusion::Success),
            CheckStatus::Queued | CheckStatus::InProgress => CheckState::InProgress,
        };
        self.checks.borrow_mut().push(HeldCheck {
            id,
            head,
            name: name.to_owned(),
            external_id: external_id.to_owned(),
            status,
            last: CheckUpdate {
                state,
                title: "earlier".to_owned(),
                summary: String::new(),
                text: String::new(),
                external_id: external_id.to_owned(),
            },
        });
        id
    }

    /// Answers a reopening write as `reopen` says.
    pub(crate) fn reopening(self, reopen: Reopen) -> Self {
        self.reopen.set(reopen);
        self
    }

    /// Every call fails.
    pub(crate) fn down(self) -> Self {
        self.down.set(true);
        self
    }

    /// Before the next update lands, another writer stamps the check `stamp`.
    pub(crate) fn rival_stamps(self, stamp: &str) -> Self {
        *self.rival_stamp.borrow_mut() = Some(stamp.to_owned());
        self
    }

    /// Just after the next update is answered, whatever its answer, another writer stamps the
    /// check `stamp`.
    pub(crate) fn rival_stamps_after(self, stamp: &str) -> Self {
        *self.rival_after.borrow_mut() = Some(stamp.to_owned());
        self
    }

    /// Every check held, in creation order.
    pub(crate) fn checks(&self) -> Vec<HeldCheck> {
        self.checks.borrow().clone()
    }

    /// Every call made, in order.
    pub(crate) fn calls(&self) -> Vec<ForgeCall> {
        self.calls.borrow().clone()
    }

    fn fresh_id(&self) -> CheckRunId {
        let next = self.next_id.get().saturating_add(1);
        self.next_id.set(next);
        CheckRunId(NonZeroU64::new(next).expect("ids start above zero"))
    }

    fn locked(&self) -> bool {
        self.refs.get(&self.lock).is_some()
    }

    fn unexpected<T>(&self, call: &'static str) -> Result<T, GitHubDown> {
        self.calls.borrow_mut().push(ForgeCall::Unexpected(call));
        Err(GitHubDown)
    }
}

/// The status a written state gives a check.
fn status_of(state: CheckState) -> CheckStatus {
    match state {
        CheckState::InProgress => CheckStatus::InProgress,
        CheckState::Completed(_) => CheckStatus::Completed,
    }
}

impl Forge for FakeGitHub<'_> {
    type Error = GitHubDown;

    fn find_or_create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: &CheckUpdate,
    ) -> Result<CheckFound, GitHubDown> {
        let locked = self.locked();
        let found = self
            .checks
            .borrow()
            .iter()
            .find(|check| check.head == *head && check.name == name.as_str())
            .cloned();
        self.calls.borrow_mut().push(ForgeCall::FindOrCreate {
            name: name.as_str().to_owned(),
            created: found.is_none() && !self.down.get(),
            locked,
        });
        if self.down.get() {
            return Err(GitHubDown);
        }
        if let Some(check) = found {
            return Ok(CheckFound::Found(CheckRun {
                id: check.id,
                external_id: check.external_id,
                status: check.status,
            }));
        }
        let id = self.fresh_id();
        let status = status_of(initial.state);
        self.checks.borrow_mut().push(HeldCheck {
            id,
            head: head.clone(),
            name: name.as_str().to_owned(),
            external_id: initial.external_id.clone(),
            status,
            last: initial.clone(),
        });
        Ok(CheckFound::Created(CheckRun {
            id,
            external_id: initial.external_id.clone(),
            status,
        }))
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        update: &CheckUpdate,
    ) -> Result<(), CompareAndSetError<GitHubDown>> {
        self.calls.borrow_mut().push(ForgeCall::Update {
            id,
            expected: expected.to_owned(),
            update: update.clone(),
            locked: self.locked(),
        });
        if self.down.get() {
            return Err(CompareAndSetError::Forge(GitHubDown));
        }
        let written = self.apply_update(id, expected, update);
        if let Some(stamp) = self.rival_after.borrow_mut().take()
            && let Some(check) = self
                .checks
                .borrow_mut()
                .iter_mut()
                .find(|check| check.id == id)
        {
            check.external_id = stamp;
        }
        written
    }

    fn upsert_comment(
        &self,
        _number: NonZeroU64,
        _marker: &str,
        _body: &str,
    ) -> Result<Upserted, GitHubDown> {
        self.unexpected("upsert_comment")
    }

    fn commit_identities(&self, _commits: &[CommitId]) -> Result<Vec<CommitIdentity>, GitHubDown> {
        self.unexpected("commit_identities")
    }

    fn reviews(&self, _number: NonZeroU64) -> Result<Vec<Review>, GitHubDown> {
        self.unexpected("reviews")
    }

    fn labels(&self, _number: NonZeroU64) -> Result<Vec<String>, GitHubDown> {
        self.unexpected("labels")
    }

    fn reactions(&self, _subject: ReactionSubject) -> Result<Vec<Reaction>, GitHubDown> {
        self.unexpected("reactions")
    }

    fn timeline(&self, _number: NonZeroU64) -> Result<Vec<TimelineEvent>, GitHubDown> {
        self.unexpected("timeline")
    }
}

impl FakeGitHub<'_> {
    /// The compare-and-set write of [`Forge::update_check_if`], once the call is recorded.
    fn apply_update(
        &self,
        id: CheckRunId,
        expected: &str,
        update: &CheckUpdate,
    ) -> Result<(), CompareAndSetError<GitHubDown>> {
        let mut checks = self.checks.borrow_mut();
        let check = checks
            .iter_mut()
            .find(|check| check.id == id)
            .ok_or(CompareAndSetError::Forge(GitHubDown))?;
        if let Some(stamp) = self.rival_stamp.borrow_mut().take() {
            check.external_id = stamp;
        }
        if check.external_id != expected {
            return Err(CompareAndSetError::Conflict {
                found: check.external_id.clone(),
            });
        }
        let reopening =
            check.status == CheckStatus::Completed && update.state == CheckState::InProgress;
        match (reopening, self.reopen.get()) {
            (true, Reopen::Rejects) => return Err(CompareAndSetError::Forge(GitHubDown)),
            (true, Reopen::KeepsCompleted) => {}
            (false, _) | (true, Reopen::Accepts) => check.status = status_of(update.state),
        }
        check.external_id.clone_from(&update.external_id);
        check.last = update.clone();
        Ok(())
    }
}

/// The clock could not be read (scripted).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("the fake clock is broken")]
pub(crate) struct ClockBroken;

/// A clock that gives its scripted instants in order, the last one forever after, and records
/// each pause instead of sleeping (TST-10).
pub(crate) struct FakeClock {
    instants: RefCell<VecDeque<LedgerTime>>,
    pauses: RefCell<Vec<u8>>,
    broken: bool,
}

impl FakeClock {
    /// A clock reading `instants` in turn.
    pub(crate) fn reading(instants: &[&str]) -> Self {
        Self {
            instants: RefCell::new(instants.iter().map(|text| at(text)).collect()),
            pauses: RefCell::new(Vec::new()),
            broken: false,
        }
    }

    /// A clock that cannot be read.
    pub(crate) fn broken() -> Self {
        Self {
            instants: RefCell::new(VecDeque::new()),
            pauses: RefCell::new(Vec::new()),
            broken: true,
        }
    }

    /// The retries `open` paused before, in order.
    pub(crate) fn pauses(&self) -> Vec<u8> {
        self.pauses.borrow().clone()
    }
}

impl OpenClock for FakeClock {
    type Error = ClockBroken;

    fn now(&self) -> Result<LedgerTime, ClockBroken> {
        if self.broken {
            return Err(ClockBroken);
        }
        let mut instants = self.instants.borrow_mut();
        match instants.len() {
            0 => Err(ClockBroken),
            1 => instants.front().copied().ok_or(ClockBroken),
            _ => instants.pop_front().ok_or(ClockBroken),
        }
    }

    fn pause(&self, retry: u8) {
        self.pauses.borrow_mut().push(retry);
    }
}
