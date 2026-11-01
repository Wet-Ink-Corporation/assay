//! Fakes for `run_republish` (TST-09): GitHub with the App's token (the PR live and the verdict
//! check) and with the run's (the default branch, decide-only's uploaded decision bundle, and
//! the evidence run's provenance and bundle),
//! each call logged by name into the log the ledger's files share, so a test can tell that no
//! port was called.

use std::cell::RefCell;
use std::num::NonZeroU64;

use assay_app::{
    BundleSource, CheckName, CheckRun, CheckRunId, CheckStatus, CommentId, CommitId,
    CurrentPullRequest, Deadline, DecisionBundleSource, PullOnHead, PullRequestState,
    RunProvenance, Sha256Digest, StoredBundle, SummaryComment, VerdictForge, WorkflowRunFacts,
};
use assay_cli::DefaultBranch;
use assay_domain::RunUid;

use crate::decide_fakes::VerdictCheck;
use crate::open_fakes::Refused;
use crate::redecide_fakes::Log;

/// The App's GitHub: the PR live at `current`, and `check` as the verdict check on its head.
pub(crate) struct Verdict<'l> {
    pub(crate) current: CurrentPullRequest,
    pub(crate) check: VerdictCheck,
    pub(crate) log: &'l Log,
}

impl Verdict<'_> {
    fn called(&self, name: &str) {
        self.log.borrow_mut().push(name.to_owned());
    }
}

/// The verdict check as a completed one written under `key`.
pub(crate) fn completed_under(key: &Sha256Digest) -> Result<VerdictCheck, Refused> {
    Ok(VerdictCheck {
        check: RefCell::new(CheckRun {
            id: CheckRunId(NonZeroU64::new(9).ok_or(Refused("not zero"))?),
            external_id: key.to_string(),
            status: CheckStatus::Completed,
        }),
        update: RefCell::new(None),
        summary: RefCell::new(None),
        calls: RefCell::new(Vec::new()),
    })
}

impl PullRequestState for Verdict<'_> {
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

impl VerdictForge for Verdict<'_> {
    type Error = Refused;

    fn find_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        deadline: Option<Deadline>,
    ) -> Result<Option<CheckRun>, Refused> {
        self.called("find check");
        self.check.find_check(head, name, deadline)
    }

    fn update_check(
        &self,
        id: CheckRunId,
        write: assay_app::LockedWrite<'_>,
    ) -> Result<(), Refused> {
        self.called("update check");
        self.check.update_check(id, write)
    }

    fn find_summary(
        &self,
        number: NonZeroU64,
        marker: &str,
        deadline: Deadline,
    ) -> Result<Option<SummaryComment>, Refused> {
        self.called("find summary");
        self.check.find_summary(number, marker, deadline)
    }

    fn create_summary(
        &self,
        number: NonZeroU64,
        body: &str,
        deadline: Deadline,
    ) -> Result<(), Refused> {
        self.called("create summary");
        self.check.create_summary(number, body, deadline)
    }

    fn update_summary(&self, id: CommentId, body: &str, deadline: Deadline) -> Result<(), Refused> {
        self.called("update summary");
        self.check.update_summary(id, body, deadline)
    }
}

/// The run token's GitHub: `main` as the default branch, `stored` as the decision bundle the
/// upload step left, and `run` and `evidence` as the evidence run's provenance and bundle, as
/// a refusal is read again.
pub(crate) struct Store<'l> {
    pub(crate) stored: StoredBundle,
    pub(crate) run: WorkflowRunFacts,
    pub(crate) evidence: StoredBundle,
    pub(crate) log: &'l Log,
}

impl RunProvenance for Store<'_> {
    type Error = Refused;

    fn workflow_run(&self, _: NonZeroU64) -> Result<Option<WorkflowRunFacts>, Refused> {
        self.log.borrow_mut().push("workflow run".to_owned());
        // The caller owns GitHub's answer.
        Ok(Some(self.run.clone()))
    }
}

impl BundleSource for Store<'_> {
    type Error = Refused;

    fn fetch(&self, _: RunUid, _: &str, _: usize) -> Result<StoredBundle, Refused> {
        self.log.borrow_mut().push("fetch evidence".to_owned());
        // The caller owns what the store answers; the fake answers the same on every call.
        Ok(self.evidence.clone())
    }
}

impl DecisionBundleSource for Store<'_> {
    type Error = Refused;

    fn fetch_decision(
        &self,
        run: RunUid,
        pr: NonZeroU64,
        _: usize,
    ) -> Result<StoredBundle, Refused> {
        self.log
            .borrow_mut()
            .push(format!("fetch decision {} {pr}", run.run_id));
        // The caller owns what the store answers; the fake answers the same on every call.
        Ok(self.stored.clone())
    }
}

impl DefaultBranch for Store<'_> {
    type Error = Refused;

    fn default_branch(&self) -> Result<String, Refused> {
        self.log.borrow_mut().push("default branch".to_owned());
        Ok("main".to_owned())
    }
}
