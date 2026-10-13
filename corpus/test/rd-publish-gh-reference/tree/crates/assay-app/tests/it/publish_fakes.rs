//! A stateful fake of GitHub, the ledger and the artifact store behind `publish`'s ports
//! (TST-09): one check run per (head, name, app) with GitHub's accumulating annotations, App and
//! user comments, a lock ref that one writer holds at a time, the PR's ledger state and the
//! uploaded artifacts. Every port call is logged so a test can tell reads from writes.

use std::cell::RefCell;
use std::error::Error;
use std::num::NonZeroU64;

use assay_app::{
    BundleStore, CheckName, CheckRun, CheckRunId, CheckStatus, CheckUpdate, CommentForge,
    CommentId, CommitId, EvidenceEntry, InlineComment, PublishLedger, SummaryComment, VerdictForge,
};
use assay_domain::Side;

use crate::plan_fakes::FakeError;

/// Who authored a check or comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Author {
    /// The `assay-verdict` App.
    App,
    /// Anyone else.
    Other,
}

/// One check run as GitHub holds it.
#[derive(Debug, Clone)]
pub(crate) struct FakeCheck {
    pub(crate) id: CheckRunId,
    pub(crate) head: String,
    pub(crate) name: String,
    pub(crate) author: Author,
    pub(crate) external_id: String,
    pub(crate) status: CheckStatus,
    /// The last update written, if any.
    pub(crate) output: Option<CheckUpdate>,
    /// Annotations accumulate on GitHub across updates; `CheckUpdate` has none to add.
    pub(crate) annotations: usize,
}

/// One issue comment.
#[derive(Debug, Clone)]
pub(crate) struct FakeComment {
    pub(crate) id: CommentId,
    pub(crate) author: Author,
    pub(crate) body: String,
}

/// One review comment on a diff line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FakeReviewComment {
    pub(crate) author: Author,
    pub(crate) commit_id: String,
    pub(crate) path: String,
    pub(crate) line: NonZeroU64,
    pub(crate) start_line: Option<NonZeroU64>,
    pub(crate) side: Side,
    pub(crate) body: String,
}

impl FakeReviewComment {
    /// A comment on line 1 of `src/lib.rs` at `head` with `body`.
    pub(crate) fn on_head(author: Author, head: &str, body: &str) -> Self {
        Self {
            author,
            commit_id: head.to_owned(),
            path: "src/lib.rs".to_owned(),
            line: NonZeroU64::MIN,
            start_line: None,
            side: Side::Right,
            body: body.to_owned(),
        }
    }
}

/// A port call, in the order made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Call {
    FindCheck,
    UpdateCheck,
    CreateCheck,
    ReadReviewComments,
    CreateReviewComment,
    FindSummary,
    CreateSummary,
    UpdateSummary,
    Acquire,
    Release,
    RecordEvidence,
    Upload,
}

impl Call {
    /// Whether the call changes anything.
    pub(crate) const fn writes(self) -> bool {
        !matches!(
            self,
            Self::FindCheck | Self::FindSummary | Self::ReadReviewComments
        )
    }
}

/// Which call fails, and on which of its occurrences (1 = the first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Failing {
    pub(crate) call: Call,
    pub(crate) nth: usize,
}

/// Runs inside `acquire`, before the lock is taken: another writer acting between this
/// writer's first read and its lock.
pub(crate) type Interleave = Box<dyn FnOnce(&FakeWorld)>;

/// Everything the publisher can see and touch.
#[derive(Default)]
pub(crate) struct FakeWorld {
    pub(crate) checks: RefCell<Vec<FakeCheck>>,
    pub(crate) comments: RefCell<Vec<FakeComment>>,
    pub(crate) review_comments: RefCell<Vec<FakeReviewComment>>,
    /// The lock's current lease, if held.
    pub(crate) lock: RefCell<Option<u64>>,
    leases: RefCell<u64>,
    /// Every evidence entry recorded, in order.
    pub(crate) evidence: RefCell<Vec<EvidenceEntry>>,
    /// Every upload: artifact, file, bytes.
    pub(crate) uploads: RefCell<Vec<(String, String, Vec<u8>)>>,
    pub(crate) calls: RefCell<Vec<Call>>,
    pub(crate) failing: RefCell<Vec<Failing>>,
    pub(crate) interleave: RefCell<Option<Interleave>>,
}

impl std::fmt::Debug for FakeWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeWorld")
            .field("checks", &self.checks)
            .field("calls", &self.calls)
            .finish_non_exhaustive()
    }
}

impl FakeWorld {
    /// A world with the App's check on `head` named `name`, stamped `external_id` and in
    /// progress, as `open` leaves it.
    pub(crate) fn with_check(
        head: &str,
        name: &str,
        external_id: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let world = Self::default();
        world.checks.borrow_mut().push(FakeCheck {
            id: CheckRunId(NonZeroU64::new(9001).ok_or("not zero")?),
            head: head.to_owned(),
            name: name.to_owned(),
            author: Author::App,
            external_id: external_id.to_owned(),
            status: CheckStatus::InProgress,
            output: None,
            annotations: 0,
        });
        Ok(world)
    }

    /// The App's check named `name`.
    pub(crate) fn app_check(&self, name: &str) -> Result<FakeCheck, Box<dyn Error>> {
        Ok(self
            .checks
            .borrow()
            .iter()
            .find(|check| check.author == Author::App && check.name == name)
            .ok_or("an App check")?
            .clone())
    }

    /// `open` re-stamping the App check for another run, under the lock, as GitHub would show
    /// it: in progress, with its output kept until the next write.
    pub(crate) fn restamp(&self, stamp: &str) {
        for check in self.checks.borrow_mut().iter_mut() {
            if check.author == Author::App {
                stamp.clone_into(&mut check.external_id);
                check.status = CheckStatus::InProgress;
            }
        }
    }

    /// Every write made, in order.
    pub(crate) fn writes(&self) -> Vec<Call> {
        self.calls
            .borrow()
            .iter()
            .copied()
            .filter(|call| call.writes())
            .collect()
    }

    /// Makes the `nth` occurrence of `call` fail.
    pub(crate) fn fail(&self, call: Call, nth: usize) {
        self.failing.borrow_mut().push(Failing { call, nth });
    }

    /// Logs `call` and fails it when told to.
    fn record(&self, call: Call) -> Result<(), FakeError> {
        let mut calls = self.calls.borrow_mut();
        calls.push(call);
        let nth = calls.iter().filter(|made| **made == call).count();
        if self.failing.borrow().contains(&Failing { call, nth }) {
            return Err(FakeError);
        }
        Ok(())
    }
}

impl VerdictForge for FakeWorld {
    type Error = FakeError;

    fn find_check(&self, head: &CommitId, name: &CheckName) -> Result<Option<CheckRun>, FakeError> {
        self.record(Call::FindCheck)?;
        Ok(self
            .checks
            .borrow()
            .iter()
            .find(|check| {
                check.author == Author::App
                    && check.head == head.as_str()
                    && check.name == name.as_str()
            })
            .map(|check| CheckRun {
                id: check.id,
                external_id: check.external_id.clone(),
                status: check.status,
            }))
    }

    fn update_check(&self, id: CheckRunId, update: &CheckUpdate) -> Result<(), FakeError> {
        self.record(Call::UpdateCheck)?;
        for check in self
            .checks
            .borrow_mut()
            .iter_mut()
            .filter(|check| check.id == id)
        {
            check.external_id.clone_from(&update.external_id);
            check.status = match update.state {
                assay_app::CheckState::InProgress => CheckStatus::InProgress,
                assay_app::CheckState::Completed(_) => CheckStatus::Completed,
            };
            check.output = Some(update.clone());
        }
        Ok(())
    }

    fn find_summary(
        &self,
        _number: NonZeroU64,
        marker: &str,
    ) -> Result<Option<SummaryComment>, FakeError> {
        self.record(Call::FindSummary)?;
        Ok(self
            .comments
            .borrow()
            .iter()
            .find(|comment| comment.author == Author::App && comment.body.contains(marker))
            .map(|comment| SummaryComment {
                id: comment.id,
                body: comment.body.clone(),
            }))
    }

    fn create_summary(&self, _number: NonZeroU64, body: &str) -> Result<(), FakeError> {
        self.record(Call::CreateSummary)?;
        let mut comments = self.comments.borrow_mut();
        let next = u64::try_from(comments.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        comments.push(FakeComment {
            id: CommentId(NonZeroU64::new(next).unwrap_or(NonZeroU64::MIN)),
            author: Author::App,
            body: body.to_owned(),
        });
        Ok(())
    }

    fn update_summary(&self, id: CommentId, body: &str) -> Result<(), FakeError> {
        self.record(Call::UpdateSummary)?;
        for comment in self.comments.borrow_mut().iter_mut().filter(|c| c.id == id) {
            body.clone_into(&mut comment.body);
        }
        Ok(())
    }
}

impl CommentForge for FakeWorld {
    fn app_review_comments(&self, _number: NonZeroU64) -> Result<Vec<String>, FakeError> {
        self.record(Call::ReadReviewComments)?;
        Ok(self
            .review_comments
            .borrow()
            .iter()
            .filter(|comment| comment.author == Author::App)
            .map(|comment| comment.body.clone())
            .collect())
    }

    fn create_review_comment(
        &self,
        _number: NonZeroU64,
        comment: &InlineComment<'_>,
    ) -> Result<(), FakeError> {
        self.record(Call::CreateReviewComment)?;
        self.review_comments.borrow_mut().push(FakeReviewComment {
            author: Author::App,
            commit_id: comment.commit_id.as_str().to_owned(),
            path: comment.path.to_owned(),
            line: comment.line,
            start_line: comment.start_line,
            side: comment.side,
            body: comment.body.clone(),
        });
        Ok(())
    }

    fn create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        update: &CheckUpdate,
    ) -> Result<(), FakeError> {
        self.record(Call::CreateCheck)?;
        let mut checks = self.checks.borrow_mut();
        let next = u64::try_from(checks.len())
            .unwrap_or(u64::MAX)
            .saturating_add(9001);
        checks.push(FakeCheck {
            id: CheckRunId(NonZeroU64::new(next).unwrap_or(NonZeroU64::MIN)),
            head: head.as_str().to_owned(),
            name: name.as_str().to_owned(),
            author: Author::App,
            external_id: update.external_id.clone(),
            status: match update.state {
                assay_app::CheckState::InProgress => CheckStatus::InProgress,
                assay_app::CheckState::Completed(_) => CheckStatus::Completed,
            },
            output: Some(update.clone()),
            annotations: 0,
        });
        Ok(())
    }
}

impl PublishLedger for FakeWorld {
    type Error = FakeError;
    type Lease = u64;

    fn acquire(&self, _repo_id: NonZeroU64, _head: &CommitId) -> Result<Option<u64>, FakeError> {
        // Taken out first, so the other writer can run every port itself.
        let interleave = self.interleave.borrow_mut().take();
        if let Some(other) = interleave {
            other(self);
        }
        self.record(Call::Acquire)?;
        let mut lock = self.lock.borrow_mut();
        if lock.is_some() {
            return Ok(None);
        }
        let mut leases = self.leases.borrow_mut();
        *leases = leases.saturating_add(1);
        *lock = Some(*leases);
        Ok(Some(*leases))
    }

    fn release(&self, lease: u64) -> Result<(), FakeError> {
        self.record(Call::Release)?;
        let mut lock = self.lock.borrow_mut();
        if *lock == Some(lease) {
            *lock = None;
        }
        Ok(())
    }

    fn record_evidence(&self, entry: &EvidenceEntry) -> Result<(), FakeError> {
        self.record(Call::RecordEvidence)?;
        self.evidence.borrow_mut().push(entry.clone());
        Ok(())
    }
}

impl BundleStore for FakeWorld {
    type Error = FakeError;

    fn upload(&self, artifact: &str, file: &str, bytes: &[u8]) -> Result<(), FakeError> {
        self.record(Call::Upload)?;
        // The port's contract: equal bytes again succeed without writing; other bytes for an
        // existing file are refused, never overwritten.
        let mut uploads = self.uploads.borrow_mut();
        match uploads.iter().find(|(a, f, _)| a == artifact && f == file) {
            Some((_, _, kept)) if kept == bytes => Ok(()),
            Some(_) => Err(FakeError),
            None => {
                uploads.push((artifact.to_owned(), file.to_owned(), bytes.to_vec()));
                Ok(())
            }
        }
    }
}
