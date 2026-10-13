//! The PR's history for re-review (`docs/architecture/04-pipeline.md` §7): how the last
//! completed run's head stands to this run's head, and `git range-diff` between them.

use super::GitError;
use super::command::GitOperation;
use super::repository::ObjectRepo;
use super::sha::CommitSha;
use crate::process::ProcessError;

/// How an earlier head stands to the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ancestry {
    /// It is an ancestor of the head, or the head itself.
    Ancestor,
    /// It is in the repository but not an ancestor: the PR was rebased or force-pushed.
    Rewritten,
    /// It is not a commit in the repository.
    Unreachable,
}

/// What `git range-diff` printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeDiffOutcome {
    /// Its output, within the limit.
    Listed(Vec<u8>),
    /// Its output was longer than the limit (or than the repository's output cap) and was not
    /// kept.
    TooLarge,
}

/// The three commits a range-diff compares: `base..previous` against `base..head`.
#[derive(Debug, Clone, Copy)]
pub struct RangeDiffCommits<'a> {
    /// The PR's base.
    pub base: &'a CommitSha,
    /// The earlier head.
    pub previous: &'a CommitSha,
    /// The current head.
    pub head: &'a CommitSha,
}

impl ObjectRepo {
    /// How `previous` stands to `head`: `cat-file -e` for its presence, then
    /// `merge-base --is-ancestor`.
    ///
    /// # Errors
    /// [`GitError`] when git cannot run, `head` is missing, or `merge-base` fails other than by
    /// answering no (exit 1).
    pub fn ancestry(&self, previous: &CommitSha, head: &CommitSha) -> Result<Ancestry, GitError> {
        let object = format!("{previous}^{{commit}}");
        match self.run(self.call(GitOperation::VerifyCommit, &[&object])) {
            Err(GitError::Failed { .. }) => return Ok(Ancestry::Unreachable),
            verified => verified.map(drop)?,
        }
        let args = [previous.as_str(), head.as_str()];
        match self.run(self.call(GitOperation::IsAncestor, &args)) {
            Ok(_) => Ok(Ancestry::Ancestor),
            Err(GitError::Failed { status, .. }) if status.code() == Some(1) => {
                Ok(Ancestry::Rewritten)
            }
            Err(error) => Err(error),
        }
    }

    /// `git range-diff <base> <previous> <head>`: the commits `previous` added on `base` paired
    /// with those `head` adds, without notes, colour, external diff drivers or textconv. Its
    /// output holds the commits' messages as well as their diffs.
    ///
    /// # Errors
    /// [`GitError`] when git cannot run or fails. Output past `limit` bytes, or past the
    /// repository's output cap, is [`RangeDiffOutcome::TooLarge`], not an error.
    pub fn range_diff(
        &self,
        commits: RangeDiffCommits<'_>,
        limit: usize,
    ) -> Result<RangeDiffOutcome, GitError> {
        let RangeDiffCommits {
            base,
            previous,
            head,
        } = commits;
        let args = [base.as_str(), previous.as_str(), head.as_str()];
        match self.run(self.call(GitOperation::RangeDiff, &args)) {
            Ok(captured) if captured.stdout.len() <= limit => {
                Ok(RangeDiffOutcome::Listed(captured.stdout))
            }
            Ok(_)
            | Err(GitError::Process {
                source: ProcessError::OutputTooLarge,
                ..
            }) => Ok(RangeDiffOutcome::TooLarge),
            Err(error) => Err(error),
        }
    }
}
