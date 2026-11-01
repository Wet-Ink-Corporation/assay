//! Fakes for `run_redecide` (TST-09): GitHub with the App's and the run's tokens as one fake
//! that logs every call, and a ledger that logs every file it reads and writes into the same
//! log, so a test can pin the order of the calls (J7) and the files written (J6).

use std::cell::RefCell;
use std::num::NonZeroU64;

use assay_app::{
    BundleSource, CheckFound, CheckName, CheckRunId, CommandComments, CommandReplies, CommentId,
    CommitId, CommitIdentity, CompareAndSetError, CurrentPullRequest, Forge, IssueComment, Ledger,
    LedgerFile, LedgerPath, LockedWrite, PullOnHead, PullRequestState, Reaction, ReactionSubject,
    Reply, Review, RunEvent, RunProvenance, StoredBundle, TimelineEvent, TipListFiles, TipLists,
    Upserted, UserId, WorkflowRunFacts, WriteCondition, WriteOutcome,
};
use assay_cli::DefaultBranch;
use assay_domain::RunUid;

use crate::decide_fakes::Reviewless;
use crate::open_fakes::{MemoryLedger, Refused};

/// The shared call log.
pub(crate) type Log = RefCell<Vec<String>>;

/// GitHub as decide-only reads it: the PR live at `current`, the base tip's `owners`, no
/// review, the pipeline's own run `4242`, and `stored` as its evidence bundle. Replies are
/// recorded, and every call is logged by name.
pub(crate) struct Hub<'l> {
    pub(crate) current: CurrentPullRequest,
    pub(crate) owners: Option<Vec<u8>>,
    pub(crate) run: WorkflowRunFacts,
    pub(crate) stored: StoredBundle,
    pub(crate) replies: RefCell<Vec<(NonZeroU64, CommentId, Reply)>>,
    /// The PR's comments, as a command record's re-fetch finds them.
    pub(crate) comments: Vec<IssueComment>,
    pub(crate) forge: Reviewless,
    pub(crate) log: &'l Log,
}

impl Hub<'_> {
    fn called(&self, name: &str) {
        self.log.borrow_mut().push(name.to_owned());
    }
}

/// GitHub's answer for the pipeline's `pull_request_target` run of `run`.
pub(crate) fn pipeline_run(run: RunUid) -> WorkflowRunFacts {
    WorkflowRunFacts {
        run_id: run.run_id,
        repo_id: run.repo_id,
        path: ".github/workflows/assay.yml".to_owned(),
        event: RunEvent::PullRequestTarget,
        head_branch: "feature".to_owned(),
        head_repo: Some(run.repo_id),
        run_attempt: NonZeroU64::MIN,
    }
}

impl PullRequestState for Hub<'_> {
    type Error = Refused;

    fn current(&self, _: NonZeroU64) -> Result<CurrentPullRequest, Refused> {
        self.called("current");
        // The caller owns what GitHub answers; the fake answers the same on every call.
        Ok(self.current.clone())
    }

    fn pulls_on_head(&self, _: &CommitId) -> Result<Vec<PullOnHead>, Refused> {
        Err(Refused("pulls on head"))
    }

    fn newer_run(&self, _: RunUid, _: &CommitId) -> Result<bool, Refused> {
        Err(Refused("newer run"))
    }
}

impl TipLists for Hub<'_> {
    type Error = Refused;

    fn lists_at(&self, _: &str) -> Result<TipListFiles, Refused> {
        self.called("lists at");
        Ok(TipListFiles {
            // The caller owns the files; the fake keeps them.
            owners: self.owners.clone(),
            agents: None,
        })
    }
}

impl Forge for Hub<'_> {
    type Error = Refused;

    fn find_or_create_check(
        &self,
        _: &CommitId,
        _: &CheckName,
        _: LockedWrite<'_>,
    ) -> Result<CheckFound, Refused> {
        Err(Refused("decide-only decide writes no check"))
    }

    fn update_check_if(
        &self,
        _: CheckRunId,
        _: &str,
        _: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<Refused>> {
        Err(CompareAndSetError::Forge(Refused(
            "decide-only decide writes no check",
        )))
    }

    fn upsert_comment(&self, _: NonZeroU64, _: &str, _: &str) -> Result<Upserted, Refused> {
        Err(Refused("decide-only decide upserts no comment"))
    }

    fn commit_identities(&self, commits: &[CommitId]) -> Result<Vec<CommitIdentity>, Refused> {
        self.forge.commit_identities(commits)
    }

    fn reviews(&self, number: NonZeroU64) -> Result<Vec<Review>, Refused> {
        self.called("reviews");
        self.forge.reviews(number)
    }

    fn labels(&self, number: NonZeroU64) -> Result<Vec<String>, Refused> {
        self.forge.labels(number)
    }

    fn reactions(&self, subject: ReactionSubject) -> Result<Vec<Reaction>, Refused> {
        self.forge.reactions(subject)
    }

    fn timeline(&self, number: NonZeroU64) -> Result<Vec<TimelineEvent>, Refused> {
        self.forge.timeline(number)
    }
}

impl CommandComments for Hub<'_> {
    type Error = Refused;

    fn comment(&self, id: CommentId) -> Result<Option<IssueComment>, Refused> {
        self.called("comment");
        // The caller owns the comment GitHub answers; the fake keeps its list.
        Ok(self
            .comments
            .iter()
            .find(|comment| comment.id == id)
            .cloned())
    }
}

impl CommandReplies for Hub<'_> {
    type Error = Refused;

    fn reply(&self, pr: NonZeroU64, to: CommentId, reply: Reply) -> Result<(), Refused> {
        self.called("reply");
        self.replies.borrow_mut().push((pr, to, reply));
        Ok(())
    }
}

impl RunProvenance for Hub<'_> {
    type Error = Refused;

    fn workflow_run(&self, _: NonZeroU64) -> Result<Option<WorkflowRunFacts>, Refused> {
        self.called("workflow run");
        // The caller owns GitHub's answer.
        Ok(Some(self.run.clone()))
    }
}

impl BundleSource for Hub<'_> {
    type Error = Refused;

    fn fetch(&self, _: RunUid, _: &str, _: usize) -> Result<StoredBundle, Refused> {
        self.called("fetch bundle");
        // The caller owns what the store answers; the fake answers the same on every call.
        Ok(self.stored.clone())
    }
}

impl DefaultBranch for Hub<'_> {
    type Error = Refused;

    fn default_branch(&self) -> Result<String, Refused> {
        self.called("default branch");
        Ok("main".to_owned())
    }
}

/// The ledger's files, every read and write logged as `read <path>` or `write <path>`; a write
/// to a path starting with `refuse` fails, after it is logged.
pub(crate) struct LoggedLedger<'l> {
    pub(crate) files: &'l MemoryLedger,
    pub(crate) log: &'l Log,
    pub(crate) refuse: Option<&'static str>,
}

impl Ledger for LoggedLedger<'_> {
    type Error = Refused;

    fn read(&self, path: &LedgerPath) -> Result<Option<LedgerFile>, Refused> {
        self.log.borrow_mut().push(format!("read {path}"));
        self.files.read(path)
    }

    fn write(
        &self,
        path: &LedgerPath,
        contents: &[u8],
        condition: &WriteCondition,
    ) -> Result<WriteOutcome, Refused> {
        let path_text = path.to_string();
        let refused = self
            .refuse
            .is_some_and(|prefix| path_text.starts_with(prefix));
        self.log.borrow_mut().push(format!("write {path_text}"));
        if refused {
            return Err(Refused("the ledger refuses this write"));
        }
        self.files.write(path, contents, condition)
    }
}

/// `id` as a user.
pub(crate) fn user(id: u64) -> Result<UserId, Refused> {
    NonZeroU64::new(id).map(UserId).ok_or(Refused("a zero id"))
}
