//! A fake of what `assay status` reads (TST-09): GitHub as the viewer sees it and the run's
//! artifact store, built from what `publish` wrote over [`FakeWorld`] so the status reads
//! exactly the check text and bundle the publisher produces.

use std::cell::RefCell;
use std::num::NonZeroU64;

use assay_app::{
    AppId, BUNDLE_FILE, BundleSource, CheckConclusion, CheckState, CheckStatus, CheckView,
    CommitId, ObservedConclusion, PullView, ReviewCommentView, Sha256Digest, StatusForge,
    StatusStep, StoredBundle, UserId, Viewer,
};
use assay_domain::RunUid;

use crate::plan_fakes::{FakeError, HEAD, run_uid};
use crate::publish_fakes::{Author, FakeWorld};

/// The repository every status is for.
pub(crate) const REPO: &str = "Wet-Ink-Corporation/assay";
/// The PR's head branch.
pub(crate) const BRANCH: &str = "feature/status";
/// When every status is read.
pub(crate) const FETCHED_AT: &str = "2026-10-13T09:30:00Z";

/// The `assay-verdict` App.
pub(crate) fn app() -> AppId {
    AppId(NonZeroU64::new(424_242).expect("not zero"))
}

/// Another app: GitHub Actions, as a PR's workflow posts.
pub(crate) fn other_app() -> AppId {
    AppId(NonZeroU64::new(15_368).expect("not zero"))
}

/// A user id.
pub(crate) fn user(id: u64) -> UserId {
    UserId(NonZeroU64::new(id).expect("test user ids are not zero"))
}

/// The page of check run `id`.
pub(crate) fn run_url(id: u64) -> String {
    format!("https://github.com/{REPO}/runs/{id}")
}

/// What the store holds for the decide job of run 7.
#[derive(Debug, Clone)]
pub(crate) enum Stored {
    Bytes(Vec<u8>),
    Missing,
    Expired,
    TooLarge,
    Fails,
}

/// GitHub and the artifact store as `status` reads them.
#[derive(Debug)]
pub(crate) struct FakeGitHub {
    pub(crate) pulls: Vec<PullView>,
    pub(crate) checks: Vec<CheckView>,
    pub(crate) comments: Vec<ReviewCommentView>,
    pub(crate) viewer: Viewer,
    pub(crate) bundle: Stored,
    /// The read that fails, if any.
    pub(crate) failing: Option<StatusStep>,
    /// Whether `review_comments` returns every comment, as an adapter that forgot to filter.
    pub(crate) unfiltered: bool,
    /// Every bundle fetch: the run, the file and the limit.
    pub(crate) fetches: RefCell<Vec<(RunUid, String, usize)>>,
}

impl FakeGitHub {
    /// PR 42 from `feature/status` into `main` at the scenario head, with no check, comment or
    /// bundle, viewed by `someone` (user 3).
    pub(crate) fn new() -> Self {
        Self {
            pulls: vec![pull(42, BRANCH)],
            checks: Vec::new(),
            comments: Vec::new(),
            viewer: Viewer {
                id: user(3),
                login: "someone".to_owned(),
            },
            bundle: Stored::Missing,
            failing: None,
            unfiltered: false,
            fetches: RefCell::new(Vec::new()),
        }
    }

    /// What `publish` left in `world`: its checks (the App's as `app()`, any other as
    /// `other_app()`) and its one uploaded bundle.
    pub(crate) fn published(world: &FakeWorld) -> Self {
        let mut github = Self::new();
        github.checks = world
            .checks
            .borrow()
            .iter()
            .map(|check| CheckView {
                app: match check.author {
                    Author::App => app(),
                    Author::Other => other_app(),
                },
                name: check.name.clone(),
                status: check.status,
                conclusion: check.output.as_ref().and_then(|output| match output.state {
                    CheckState::InProgress => None,
                    CheckState::Completed(conclusion) => Some(observed(conclusion)),
                }),
                external_id: Some(check.external_id.clone()),
                output_text: check.output.as_ref().map(|output| output.text.clone()),
                url: run_url(check.id.0.get()),
            })
            .collect();
        github.bundle = match world.uploads.borrow().as_slice() {
            [(_, _, bytes)] => Stored::Bytes(bytes.clone()),
            _ => Stored::Missing,
        };
        github
    }

    /// Adds the App's check `assay / gate: <plugin>`, completed with `conclusion`.
    pub(crate) fn with_gate(
        mut self,
        plugin: &str,
        conclusion: ObservedConclusion,
        id: u64,
    ) -> Self {
        self.checks.push(CheckView {
            app: app(),
            name: format!("assay / gate: {plugin}"),
            status: CheckStatus::Completed,
            conclusion: Some(conclusion),
            external_id: None,
            output_text: None,
            url: run_url(id),
        });
        self
    }

    /// Adds the App's inline comment `body` at `(path, line)`, with page `url_id`.
    pub(crate) fn with_comment(
        mut self,
        body: &str,
        (path, line): (&str, u64),
        url_id: u64,
    ) -> Self {
        self.comments.push(ReviewCommentView {
            app: Some(app()),
            body: body.to_owned(),
            path: path.to_owned(),
            line: NonZeroU64::new(line),
            url: format!("https://github.com/{REPO}/pull/42#discussion_r{url_id}"),
        });
        self
    }

    /// The viewer becomes `login` with id `id`.
    pub(crate) fn viewed_by(mut self, id: u64, login: &str) -> Self {
        self.viewer = Viewer {
            id: user(id),
            login: login.to_owned(),
        };
        self
    }

    fn fail(&self, step: StatusStep) -> Result<(), FakeError> {
        if self.failing == Some(step) {
            Err(FakeError)
        } else {
            Ok(())
        }
    }
}

/// PR `number` from `branch` into `main` at the scenario head.
pub(crate) fn pull(number: u64, branch: &str) -> PullView {
    PullView {
        number: NonZeroU64::new(number).expect("PR numbers are not zero"),
        head_sha: HEAD.parse().expect("the scenario head is a commit id"),
        head_ref: branch.to_owned(),
        base_ref: "main".to_owned(),
    }
}

fn observed(conclusion: CheckConclusion) -> ObservedConclusion {
    match conclusion {
        CheckConclusion::Success => ObservedConclusion::Success,
        CheckConclusion::Failure => ObservedConclusion::Failure,
        CheckConclusion::Neutral => ObservedConclusion::Neutral,
    }
}

impl StatusForge for FakeGitHub {
    type Error = FakeError;

    fn pull(&self, number: NonZeroU64) -> Result<Option<PullView>, FakeError> {
        self.fail(StatusStep::Pull)?;
        Ok(self
            .pulls
            .iter()
            .find(|pull| pull.number == number)
            .cloned())
    }

    fn open_pulls(&self, branch: &str) -> Result<Vec<PullView>, FakeError> {
        self.fail(StatusStep::OpenPulls)?;
        Ok(self
            .pulls
            .iter()
            .filter(|pull| pull.head_ref == branch)
            .cloned()
            .collect())
    }

    fn check_runs(&self, head: &CommitId) -> Result<Vec<CheckView>, FakeError> {
        self.fail(StatusStep::CheckRuns)?;
        assert_eq!(head.as_str(), HEAD, "checks are read on the PR's head");
        Ok(self.checks.clone())
    }

    fn review_comments(
        &self,
        _number: NonZeroU64,
        app: AppId,
    ) -> Result<Vec<ReviewCommentView>, FakeError> {
        self.fail(StatusStep::ReviewComments)?;
        Ok(self
            .comments
            .iter()
            .filter(|comment| self.unfiltered || comment.app == Some(app))
            .cloned()
            .collect())
    }

    fn viewer(&self) -> Result<Viewer, FakeError> {
        self.fail(StatusStep::Viewer)?;
        Ok(self.viewer.clone())
    }
}

impl BundleSource for FakeGitHub {
    type Error = FakeError;

    fn fetch(&self, run: RunUid, file: &str, limit: usize) -> Result<StoredBundle, FakeError> {
        self.fetches
            .borrow_mut()
            .push((run, file.to_owned(), limit));
        if file != BUNDLE_FILE || run != run_uid(1).map_err(|_not_a_run| FakeError)? {
            return Ok(StoredBundle::Missing);
        }
        match &self.bundle {
            Stored::Bytes(bytes) => Ok(StoredBundle::Found(bytes.clone())),
            Stored::Missing => Ok(StoredBundle::Missing),
            Stored::Expired => Ok(StoredBundle::Expired),
            Stored::TooLarge => Ok(StoredBundle::TooLarge),
            Stored::Fails => Err(FakeError),
        }
    }
}

impl FakeGitHub {
    /// The App's verdict check.
    pub(crate) fn verdict_mut(&mut self) -> &mut CheckView {
        self.checks
            .iter_mut()
            .find(|check| check.app == app() && check.name == "assay / verdict")
            .expect("the App's verdict check")
    }

    /// Stores `bytes` as the bundle and points the verdict check's evidence at them, as if the
    /// decide job had uploaded them.
    pub(crate) fn repoint(&mut self, bytes: Vec<u8>) {
        let Stored::Bytes(old) = &self.bundle else {
            panic!("repoint needs a persisted bundle");
        };
        let old = Sha256Digest::of(old).to_string();
        let new = Sha256Digest::of(&bytes).to_string();
        let check = self.verdict_mut();
        let text = check
            .output_text
            .as_deref()
            .expect("a decided check's text");
        check.output_text = Some(text.replace(&old, &new));
        self.bundle = Stored::Bytes(bytes);
    }

    /// The persisted bundle, parsed.
    pub(crate) fn bundle_value(&self) -> serde_json::Value {
        let Stored::Bytes(bytes) = &self.bundle else {
            panic!("no persisted bundle");
        };
        serde_json::from_slice(bytes).expect("the bundle is JSON")
    }
}
