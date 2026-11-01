//! `assay status`'s read port, with the viewer's own token: PRs, check runs of every app, the
//! App's review comments and the viewer; and the repository itself, which the status names.

use std::num::NonZeroU64;

use assay_app::{
    AppId, CheckStatus, CheckView, CommitId, ObservedConclusion, PullView, ReviewCommentView,
    StatusForge, StatusRepository, UserId, Viewer,
};

use super::api::{PER_PAGE, percent_encode};
use super::checks::check_status;
use super::dto::{self, CheckRunDto, PullDto, RepositoryDto, ViewerDto};
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};
use super::{GitHub, api_path};

impl<T: Transport, S: Sleep> StatusForge for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn pull(&self, number: NonZeroU64) -> Result<Option<PullView>, Self::Error> {
        let operation = Operation::ReadPull;
        let path = self.path(operation, &format!("pulls/{number}"))?;
        self.api
            .get_optional::<PullDto>(operation, &path)?
            .map(|pull| pull_view(operation, pull))
            .transpose()
    }

    /// Reads every open PR on `branch` before keeping the repository's own, so that a list cut
    /// at `limit` is never cut before the filter.
    fn open_pulls(&self, branch: &str, limit: usize) -> Result<Vec<PullView>, Self::Error> {
        let operation = Operation::ListPulls;
        let path = self.path(
            operation,
            &format!(
                "pulls?state=open&head={}:{}&per_page={PER_PAGE}",
                self.repository.owner(),
                percent_encode(branch)
            ),
        )?;
        let mut pulls = Vec::new();
        for pull in self.api.list::<Vec<PullDto>>(operation, path, None)? {
            let own = pull
                .head
                .repo
                .as_ref()
                .is_some_and(|repo| self.repository.is(&repo.full_name));
            if own && pull.head.branch == branch {
                pulls.push(pull_view(operation, pull)?);
            }
        }
        pulls.truncate(limit);
        Ok(pulls)
    }

    fn check_runs(&self, head: &CommitId, limit: usize) -> Result<Vec<CheckView>, Self::Error> {
        let mut views = self
            .list_checks(head, None, Some(limit))?
            .into_iter()
            .map(|check| self.check_view(check))
            .collect::<Result<Vec<_>, _>>()?;
        views.truncate(limit);
        Ok(views)
    }

    fn review_comments(
        &self,
        number: NonZeroU64,
        app: AppId,
        limit: usize,
    ) -> Result<Vec<ReviewCommentView>, Self::Error> {
        let operation = Operation::ListReviewComments;
        if app != self.identity.app {
            return Err(ForgeError::UnknownApp { app });
        }
        let mut views = Vec::new();
        // `assay status` reads outside any lock.
        for comment in self.review_comments_of(number, None)? {
            if !comment.user.as_ref().is_some_and(|user| self.is_bot(user)) {
                continue;
            }
            views.push(ReviewCommentView {
                app: Some(app),
                body: comment.body,
                path: comment.path,
                line: comment
                    .line
                    .map(|line| dto::nonzero(operation, line, "a line is zero"))
                    .transpose()?,
                url: comment.html_url,
            });
        }
        views.truncate(limit);
        Ok(views)
    }

    fn viewer(&self) -> Result<Viewer, Self::Error> {
        let operation = Operation::ReadViewer;
        let viewer: ViewerDto = self
            .api
            .get(operation, &api_path(operation, "user".to_owned())?)?;
        if viewer.login.is_empty() {
            return Err(ForgeError::Malformed {
                operation,
                detail: "the viewer has no login",
            });
        }
        Ok(Viewer {
            id: UserId(dto::nonzero(operation, viewer.id, "a user id is zero")?),
            login: viewer.login,
        })
    }
}

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// The repository as `assay status` names it: its numeric id, which keys its runs'
    /// artifacts, its default branch, which names the verdict check, and its `owner/name` as
    /// this adapter was configured with.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Malformed`] when GitHub answers for another repository (a rename, which
    /// it redirects), with a zero id or with no default branch; [`ForgeError`] when it cannot
    /// be read.
    pub fn status_repository(&self) -> Result<StatusRepository, ForgeError<T::Error>> {
        let operation = Operation::ReadRepository;
        let path = api_path(operation, format!("repos/{}", self.repository))?;
        let seen: RepositoryDto = self.api.get(operation, &path)?;
        let malformed = |detail| ForgeError::Malformed { operation, detail };
        if !self.repository.is(&seen.full_name) {
            return Err(malformed("the API answered for another repository"));
        }
        let id = dto::nonzero(operation, seen.id, "a repository id is zero")?;
        if seen.default_branch.is_empty() {
            return Err(malformed("the repository has no default branch"));
        }
        Ok(StatusRepository {
            id,
            name: self.repository.to_string(),
            default_branch: seen.default_branch,
        })
    }

    /// `check` as `status` sees it. The App's own check must have one of its three statuses;
    /// another app's pre-start states (`waiting`, `requested`, `pending`, GitHub Actions') read
    /// as queued, since `status` keeps only the App's.
    fn check_view(&self, check: CheckRunDto) -> Result<CheckView, ForgeError<T::Error>> {
        let operation = Operation::ListChecks;
        let app = dto::app(operation, check.app.as_ref())?;
        let status = match check.status.as_str() {
            "waiting" | "requested" | "pending" if app != self.identity.app => CheckStatus::Queued,
            other => check_status(operation, other)?,
        };
        Ok(CheckView {
            app,
            name: check.name,
            status,
            conclusion: check
                .conclusion
                .as_deref()
                .map(|conclusion| observed_conclusion(operation, conclusion))
                .transpose()?,
            external_id: check
                .external_id
                .filter(|external_id| !external_id.is_empty()),
            output_title: check.output.title,
            output_text: check.output.text,
            url: check.html_url.ok_or(ForgeError::Malformed {
                operation,
                detail: "a check run has no html_url",
            })?,
        })
    }
}

/// A PR as `status` sees it.
fn pull_view<E>(operation: Operation, pull: PullDto) -> Result<PullView, ForgeError<E>> {
    Ok(PullView {
        number: dto::nonzero(operation, pull.number, "a PR number is zero")?,
        head_sha: dto::commit(operation, &pull.head.sha)?,
        head_ref: pull.head.branch,
        base_ref: pull.base.branch,
    })
}

/// A conclusion of any app's check run.
pub(super) fn observed_conclusion<E>(
    operation: Operation,
    conclusion: &str,
) -> Result<ObservedConclusion, ForgeError<E>> {
    match conclusion {
        "success" => Ok(ObservedConclusion::Success),
        "failure" => Ok(ObservedConclusion::Failure),
        "neutral" => Ok(ObservedConclusion::Neutral),
        "cancelled" => Ok(ObservedConclusion::Cancelled),
        "skipped" => Ok(ObservedConclusion::Skipped),
        "timed_out" => Ok(ObservedConclusion::TimedOut),
        "action_required" => Ok(ObservedConclusion::ActionRequired),
        "stale" => Ok(ObservedConclusion::Stale),
        _ => Err(ForgeError::Malformed {
            operation,
            detail: "unknown check conclusion",
        }),
    }
}
