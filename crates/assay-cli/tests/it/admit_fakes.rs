//! Fakes for `run_admit` (TST-09): a port that records every call and answers none, GitHub
//! answering the range's commit identities, an empty artifact store and comment list, a fetch
//! that records what it was asked, and a clock fixed at one instant that a wait moves on.

use std::cell::RefCell;
use std::num::NonZeroU64;
use std::time::{Duration, SystemTime};

use assay_app::{
    Actor, BundleSource, CheckFound, CheckName, CheckRunId, Clock, CommandComments, CommentId,
    CommitId, CommitIdentity, CompareAndSetError, Forge, IssueComment, LeaseRef, Ledger,
    LedgerFile, LedgerPath, LedgerRefs, LockedWrite, Reaction, ReactionSubject, RefCreation,
    RefDeletion, RefTip, Review, StoredBundle, TimelineEvent, Upserted, UserId, WriteCondition,
    WriteOutcome,
};
use assay_cli::FetchCommits;
use assay_domain::RunUid;

use crate::open_fakes::{MemoryLedger, MemoryRefs, Refused};

/// A ledger that refuses every write once `refs` holds a ref: admit fails after it took the
/// slot, when it writes the `reserved` record.
pub(crate) struct ShutOnceHeld<'a> {
    /// The files, read and written while no slot is held.
    pub(crate) ledger: &'a MemoryLedger,
    /// The slots' refs.
    pub(crate) refs: &'a MemoryRefs,
}

impl Ledger for ShutOnceHeld<'_> {
    type Error = Refused;

    fn read(&self, path: &LedgerPath) -> Result<Option<LedgerFile>, Refused> {
        self.ledger.read(path)
    }

    fn write(
        &self,
        path: &LedgerPath,
        contents: &[u8],
        condition: &WriteCondition,
    ) -> Result<WriteOutcome, Refused> {
        if !self.refs.names().is_empty() {
            return Err(Refused("a ledger write once a slot is held"));
        }
        self.ledger.write(path, contents, condition)
    }
}

/// 2026-10-31T12:00:00Z, where the fixed clock stands.
pub(crate) const NOW_SECS: u64 = 1_793_448_000;

/// A port that answers no call and records each one by name.
#[derive(Debug, Default)]
pub(crate) struct Untouched {
    calls: RefCell<Vec<&'static str>>,
}

impl Untouched {
    /// Every call made, in order.
    pub(crate) fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }

    fn refuse<T>(&self, call: &'static str) -> Result<T, Refused> {
        self.calls.borrow_mut().push(call);
        Err(Refused(call))
    }
}

impl Ledger for Untouched {
    type Error = Refused;

    fn read(&self, _: &LedgerPath) -> Result<Option<LedgerFile>, Refused> {
        self.refuse("ledger read")
    }

    fn write(&self, _: &LedgerPath, _: &[u8], _: &WriteCondition) -> Result<WriteOutcome, Refused> {
        self.refuse("ledger write")
    }
}

impl LedgerRefs for Untouched {
    type Error = Refused;

    fn create(&self, _: &LeaseRef, _: &str) -> Result<RefCreation, Refused> {
        self.refuse("ref create")
    }

    fn read(&self, _: &LeaseRef) -> Result<Option<RefTip>, Refused> {
        self.refuse("ref read")
    }

    fn delete_if(&self, _: &LeaseRef, _: &CommitId) -> Result<RefDeletion, Refused> {
        self.refuse("ref delete")
    }
}

impl BundleSource for Untouched {
    type Error = Refused;

    fn fetch(&self, _: RunUid, _: &str, _: usize) -> Result<StoredBundle, Refused> {
        self.refuse("bundle fetch")
    }
}

impl CommandComments for Untouched {
    type Error = Refused;

    fn comment(&self, _: CommentId) -> Result<Option<IssueComment>, Refused> {
        self.refuse("comment")
    }
}

impl FetchCommits for Untouched {
    type Error = Refused;

    fn fetch(&self, _: &CommitId) -> Result<(), Refused> {
        self.refuse("git fetch")
    }
}

impl Forge for Untouched {
    type Error = Refused;

    fn find_or_create_check(
        &self,
        _: &CommitId,
        _: &CheckName,
        _: LockedWrite<'_>,
    ) -> Result<CheckFound, Refused> {
        self.refuse("check")
    }

    fn update_check_if(
        &self,
        _: CheckRunId,
        _: &str,
        _: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<Refused>> {
        self.refuse("check update")
            .map_err(CompareAndSetError::Forge)
    }

    fn upsert_comment(&self, _: NonZeroU64, _: &str, _: &str) -> Result<Upserted, Refused> {
        self.refuse("upsert comment")
    }

    fn commit_identities(&self, _: &[CommitId]) -> Result<Vec<CommitIdentity>, Refused> {
        self.refuse("commit identities")
    }

    fn reviews(&self, _: NonZeroU64) -> Result<Vec<Review>, Refused> {
        self.refuse("reviews")
    }

    fn labels(&self, _: NonZeroU64) -> Result<Vec<String>, Refused> {
        self.refuse("labels")
    }

    fn reactions(&self, _: ReactionSubject) -> Result<Vec<Reaction>, Refused> {
        self.refuse("reactions")
    }

    fn timeline(&self, _: NonZeroU64) -> Result<Vec<TimelineEvent>, Refused> {
        self.refuse("timeline")
    }
}

/// GitHub resolving every commit as authored and committed by `author`; nothing else.
pub(crate) struct Commits {
    pub(crate) author: UserId,
    pub(crate) rest: Untouched,
}

impl Forge for Commits {
    type Error = Refused;

    fn find_or_create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: LockedWrite<'_>,
    ) -> Result<CheckFound, Refused> {
        self.rest.find_or_create_check(head, name, initial)
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        write: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<Refused>> {
        self.rest.update_check_if(id, expected, write)
    }

    fn upsert_comment(
        &self,
        number: NonZeroU64,
        marker: &str,
        body: &str,
    ) -> Result<Upserted, Refused> {
        self.rest.upsert_comment(number, marker, body)
    }

    fn commit_identities(&self, commits: &[CommitId]) -> Result<Vec<CommitIdentity>, Refused> {
        Ok(commits
            .iter()
            .map(|commit| CommitIdentity {
                commit: commit.clone(),
                authors: vec![Actor::User(self.author)],
                committer: Actor::User(self.author),
            })
            .collect())
    }

    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, Refused> {
        self.rest.reviews(number)
    }

    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, Refused> {
        self.rest.labels(number)
    }

    fn reactions(&self, subject: ReactionSubject) -> Result<Vec<Reaction>, Refused> {
        self.rest.reactions(subject)
    }

    fn timeline(&self, number: NonZeroU64) -> Result<Vec<TimelineEvent>, Refused> {
        self.rest.timeline(number)
    }
}

/// An artifact store holding no bundle, recording each run asked for.
#[derive(Debug, Default)]
pub(crate) struct NoBundles {
    pub(crate) asked: RefCell<Vec<RunUid>>,
}

impl BundleSource for NoBundles {
    type Error = Refused;

    fn fetch(&self, run: RunUid, _: &str, _: usize) -> Result<StoredBundle, Refused> {
        self.asked.borrow_mut().push(run);
        Ok(StoredBundle::Missing)
    }
}

/// A PR whose every comment is deleted.
#[derive(Debug, Default)]
pub(crate) struct NoComments;

impl CommandComments for NoComments {
    type Error = Refused;

    fn comment(&self, _: CommentId) -> Result<Option<IssueComment>, Refused> {
        Ok(None)
    }
}

/// A fetch that records each commit and leaves the fixture repository as it is.
#[derive(Debug, Default)]
pub(crate) struct Fetches {
    pub(crate) asked: RefCell<Vec<CommitId>>,
}

impl FetchCommits for Fetches {
    type Error = Refused;

    fn fetch(&self, commit: &CommitId) -> Result<(), Refused> {
        self.asked.borrow_mut().push(commit.clone());
        Ok(())
    }
}

/// A clock at [`NOW_SECS`]; a wait moves it on instead of sleeping (TST-10).
#[derive(Debug)]
pub(crate) struct StillClock {
    pub(crate) now: SystemTime,
}

impl StillClock {
    /// The clock at [`NOW_SECS`].
    pub(crate) fn new() -> Self {
        Self {
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS),
        }
    }
}

impl Clock for StillClock {
    fn now(&self) -> SystemTime {
        self.now
    }

    fn sleep(&mut self, duration: Duration) {
        self.now = self
            .now
            .checked_add(duration)
            .expect("a ten-minute wait stays in range");
    }
}
