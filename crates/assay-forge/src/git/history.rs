//! The PR's history for re-review (`docs/architecture/04-pipeline.md` §7): how the last
//! completed run's head stands to this run's head, and `git range-diff` between them.

use std::time::Duration;

use super::GitError;
use super::command::GitOperation;
use super::repository::{Call, ObjectRepo};
use super::sha::CommitSha;
use crate::process::ProcessError;

/// Most commits either side of a range-diff may hold (`base..previous`, `base..head`).
///
/// Past it the range-diff is [`RangeDiffOutcome::TooLarge`] without running: range-diff pairs
/// commits by a cost matrix, quadratic in their number (H-13).
pub const MAX_RANGE_DIFF_COMMITS: u64 = 250;

/// The wall-clock cap a re-review gives range-diff: it only marks the increment, so it is cut
/// off well before a whole git process's timeout.
pub const RANGE_DIFF_TIMEOUT: Duration = Duration::from_secs(60);

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
    /// A side held more than [`MAX_RANGE_DIFF_COMMITS`] commits, so it did not run, or its
    /// output was longer than the limit (or than the repository's output cap) and was not kept.
    TooLarge,
    /// It ran past its timeout and was killed.
    TooSlow,
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
    /// How `previous` stands to `head`: `rev-parse --verify --quiet <previous>^{commit}` for its
    /// presence, then `merge-base --is-ancestor`.
    ///
    /// # Errors
    /// [`GitError`] when git cannot run, `head` is missing, or either command fails other than
    /// by answering no (exit 1): a broken object store is an error, not an unreachable commit.
    pub fn ancestry(&self, previous: &CommitSha, head: &CommitSha) -> Result<Ancestry, GitError> {
        let object = format!("{previous}^{{commit}}");
        let verify = ["--verify", "--quiet", object.as_str()];
        match self.run(self.call(GitOperation::RevParse, &verify)) {
            Err(GitError::Failed { status, .. }) if status.code() == Some(1) => {
                return Ok(Ancestry::Unreachable);
            }
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
    /// Each side is counted first (`rev-list --count`); past [`MAX_RANGE_DIFF_COMMITS`] commits
    /// it does not run. It runs within `cap` (callers pass [`RANGE_DIFF_TIMEOUT`]), not the
    /// repository's timeout.
    ///
    /// # Errors
    /// [`GitError`] when git cannot run or fails, or a count is not a number. Too many commits,
    /// output past `limit` bytes or past the repository's output cap is
    /// [`RangeDiffOutcome::TooLarge`], and running past `cap` [`RangeDiffOutcome::TooSlow`],
    /// not an error.
    pub fn range_diff(
        &self,
        commits: RangeDiffCommits<'_>,
        limit: usize,
        cap: Duration,
    ) -> Result<RangeDiffOutcome, GitError> {
        let RangeDiffCommits {
            base,
            previous,
            head,
        } = commits;
        for side in [previous, head] {
            if self.commits_between(base, side)? > MAX_RANGE_DIFF_COMMITS {
                return Ok(RangeDiffOutcome::TooLarge);
            }
        }
        let args = [base.as_str(), previous.as_str(), head.as_str()];
        match self.run(Call {
            timeout: cap,
            ..self.call(GitOperation::RangeDiff, &args)
        }) {
            Ok(captured) if captured.stdout.len() <= limit => {
                Ok(RangeDiffOutcome::Listed(captured.stdout))
            }
            Ok(_)
            | Err(GitError::Process {
                source: ProcessError::OutputTooLarge,
                ..
            }) => Ok(RangeDiffOutcome::TooLarge),
            Err(GitError::Process {
                source: ProcessError::TimedOut,
                ..
            }) => Ok(RangeDiffOutcome::TooSlow),
            Err(error) => Err(error),
        }
    }

    /// How many commits `base..tip` holds.
    fn commits_between(&self, base: &CommitSha, tip: &CommitSha) -> Result<u64, GitError> {
        let range = format!("{base}..{tip}");
        let captured = self.run(self.call(GitOperation::RevListCount, &[&range]))?;
        str::from_utf8(&captured.stdout)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .and_then(|count| count.parse().ok())
            .ok_or(GitError::Malformed {
                operation: GitOperation::RevListCount,
                detail: "rev-list --count did not print one number",
            })
    }
}
