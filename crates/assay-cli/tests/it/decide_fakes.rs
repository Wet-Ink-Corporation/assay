//! Fakes for `run_decide` and `run_publish` (TST-09): GitHub's live state of the PR, GitHub
//! answering the range's identities and no review, the verdict check `open` left stamped, the
//! per-head lock, and an artifact store holding what the upload step stored.

use std::cell::RefCell;
use std::num::NonZeroU64;

use assay_app::{
    BundleSource, CheckFound, CheckName, CheckRun, CheckRunId, CheckStatus, CheckUpdate, Claimed,
    CommentId, CommitId, CommitIdentity, CompareAndSetError, CurrentPullRequest, Deadline,
    DecisionEntry, EvidenceEntry, Forge, LedgerTime, LockedWrite, PublishLedger, PublishedEntry,
    PullOnHead, PullRequestState, Reaction, ReactionSubject, Review, StoredBundle, SummaryComment,
    TimelineEvent, Upserted, UserId, VerdictForge,
};
use assay_domain::RunUid;

use crate::admit_fakes::{Commits, Untouched};
use crate::open_fakes::Refused;

/// GitHub showing the PR at `current`, the only PR on its head, with no newer run.
pub(crate) struct LiveState {
    pub(crate) current: CurrentPullRequest,
    pub(crate) calls: RefCell<Vec<&'static str>>,
}

impl PullRequestState for LiveState {
    type Error = Refused;

    fn current(&self, _: NonZeroU64) -> Result<CurrentPullRequest, Refused> {
        self.calls.borrow_mut().push("current");
        // The caller owns what GitHub answers; the fake answers the same on every call.
        Ok(self.current.clone())
    }

    fn pulls_on_head(&self, _: &CommitId) -> Result<Vec<PullOnHead>, Refused> {
        self.calls.borrow_mut().push("pulls on head");
        Ok(Vec::new())
    }

    fn newer_run(&self, _: RunUid, _: &CommitId) -> Result<bool, Refused> {
        self.calls.borrow_mut().push("newer run");
        Ok(false)
    }
}

/// GitHub resolving every commit as `author`'s, with no review on the PR; nothing else.
pub(crate) struct Reviewless(pub(crate) Commits);

impl Reviewless {
    /// GitHub for a PR whose commits are all `author`'s.
    pub(crate) fn by(author: UserId) -> Self {
        Self(Commits {
            author,
            rest: Untouched::default(),
        })
    }
}

impl Forge for Reviewless {
    type Error = Refused;

    fn find_or_create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: LockedWrite<'_>,
    ) -> Result<CheckFound, Refused> {
        self.0.find_or_create_check(head, name, initial)
    }

    fn update_check_if(
        &self,
        id: CheckRunId,
        expected: &str,
        write: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<Refused>> {
        self.0.update_check_if(id, expected, write)
    }

    fn upsert_comment(
        &self,
        number: NonZeroU64,
        marker: &str,
        body: &str,
    ) -> Result<Upserted, Refused> {
        self.0.upsert_comment(number, marker, body)
    }

    fn commit_identities(&self, commits: &[CommitId]) -> Result<Vec<CommitIdentity>, Refused> {
        self.0.commit_identities(commits)
    }

    fn reviews(&self, _: NonZeroU64) -> Result<Vec<Review>, Refused> {
        Ok(Vec::new())
    }

    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, Refused> {
        self.0.labels(number)
    }

    fn reactions(&self, subject: ReactionSubject) -> Result<Vec<Reaction>, Refused> {
        self.0.reactions(subject)
    }

    fn timeline(&self, number: NonZeroU64) -> Result<Vec<TimelineEvent>, Refused> {
        self.0.timeline(number)
    }
}

/// The App's verdict check on the head, as `open` left it, and the summary comment once one is
/// written; every write is recorded by name.
#[derive(Debug)]
pub(crate) struct VerdictCheck {
    pub(crate) check: RefCell<CheckRun>,
    pub(crate) update: RefCell<Option<CheckUpdate>>,
    pub(crate) summary: RefCell<Option<String>>,
    pub(crate) calls: RefCell<Vec<&'static str>>,
}

impl VerdictCheck {
    /// The check stamped `in_progress` by `open` for run `run_id`.
    pub(crate) fn stamped(run_id: u64) -> Result<Self, &'static str> {
        Ok(Self {
            check: RefCell::new(CheckRun {
                id: CheckRunId(NonZeroU64::new(9).ok_or("not zero")?),
                external_id: format!("pending:{run_id}:1"),
                status: CheckStatus::InProgress,
            }),
            update: RefCell::new(None),
            summary: RefCell::new(None),
            calls: RefCell::new(Vec::new()),
        })
    }

    /// The last update written.
    pub(crate) fn written(&self) -> Option<CheckUpdate> {
        // A copy for the assertion; the fake keeps the record.
        self.update.borrow().clone()
    }
}

impl VerdictForge for VerdictCheck {
    type Error = Refused;

    fn find_check(
        &self,
        _: &CommitId,
        _: &CheckName,
        _: Option<Deadline>,
    ) -> Result<Option<CheckRun>, Refused> {
        self.calls.borrow_mut().push("find check");
        // The caller owns the check it read; the fake keeps the live one.
        Ok(Some(self.check.borrow().clone()))
    }

    fn update_check(&self, _: CheckRunId, write: LockedWrite<'_>) -> Result<(), Refused> {
        self.calls.borrow_mut().push("update check");
        let mut check = self.check.borrow_mut();
        check.external_id.clone_from(&write.update.external_id);
        check.status = CheckStatus::Completed;
        // The write is borrowed; the fake records its own copy.
        *self.update.borrow_mut() = Some(write.update.clone());
        Ok(())
    }

    fn find_summary(
        &self,
        _: NonZeroU64,
        _: &str,
        _: Deadline,
    ) -> Result<Option<SummaryComment>, Refused> {
        self.calls.borrow_mut().push("find summary");
        Ok(None)
    }

    fn create_summary(&self, _: NonZeroU64, body: &str, _: Deadline) -> Result<(), Refused> {
        self.calls.borrow_mut().push("create summary");
        *self.summary.borrow_mut() = Some(body.to_owned());
        Ok(())
    }

    fn update_summary(&self, _: CommentId, body: &str, _: Deadline) -> Result<(), Refused> {
        self.calls.borrow_mut().push("update summary");
        *self.summary.borrow_mut() = Some(body.to_owned());
        Ok(())
    }
}

/// The per-head lock, always free, and the evidence recorded under it.
#[derive(Debug, Default)]
pub(crate) struct FreeLock {
    pub(crate) evidence: RefCell<Vec<EvidenceEntry>>,
    pub(crate) calls: RefCell<Vec<&'static str>>,
}

impl PublishLedger for FreeLock {
    type Error = Refused;
    type Lease = ();

    fn acquire(&self, _: NonZeroU64, _: &CommitId) -> Result<Option<()>, Refused> {
        self.calls.borrow_mut().push("acquire");
        Ok(Some(()))
    }

    fn release(&self, (): ()) -> Result<(), Refused> {
        self.calls.borrow_mut().push("release");
        Ok(())
    }

    fn deadline(&self, (): &()) -> Deadline {
        // A lease far in the future: nothing here is bounded by it.
        Deadline::at(
            "2099-01-01T00:00:00.000Z"
                .parse::<LedgerTime>()
                .expect("a ledger time"),
        )
    }

    fn record_evidence(&self, (): &(), entry: &EvidenceEntry) -> Result<(), Refused> {
        self.calls.borrow_mut().push("record evidence");
        // The entry is borrowed; the fake records its own copy.
        self.evidence.borrow_mut().push(entry.clone());
        Ok(())
    }

    fn record_published(&self, (): &(), _: &PublishedEntry) -> Result<Claimed, Refused> {
        self.calls.borrow_mut().push("record published");
        Ok(Claimed::Recorded)
    }

    fn record_decision(&self, (): &(), _: &DecisionEntry) -> Result<Claimed, Refused> {
        self.calls.borrow_mut().push("record decision");
        Ok(Claimed::Recorded)
    }
}

/// The artifact store as the upload step left it: `stored` as the decide artifact's bundle.
#[derive(Debug)]
pub(crate) struct Uploaded {
    pub(crate) stored: StoredBundle,
    pub(crate) asked: RefCell<Vec<RunUid>>,
}

impl BundleSource for Uploaded {
    type Error = Refused;

    fn fetch(&self, run: RunUid, _: &str, _: usize) -> Result<StoredBundle, Refused> {
        self.asked.borrow_mut().push(run);
        // The caller owns what the store answers; the fake answers the same on every call.
        Ok(self.stored.clone())
    }
}
