//! The PR range for the eligibility rule (`docs/architecture/04-pipeline.md` §5): the commits
//! of `base..head` with their parents, computed from git objects in Z4 and never from GitHub's
//! 250-commit list (R3-22), and what a conflict-free merge of two commits writes, to recognise
//! an "Update branch" merge (R3-07).

use super::GitError;
use super::command::GitOperation;
use super::repository::ObjectRepo;
use super::sha::CommitSha;
use super::tree;

/// One commit of a range and its parents, the first parent first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCommit {
    /// The commit.
    pub commit: CommitSha,
    /// Its parents.
    pub parents: Vec<CommitSha>,
}

/// What [`ObjectRepo::range_commits`] listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeCommits {
    /// Every commit of the range, newest first.
    Listed(Vec<RangeCommit>),
    /// More commits than the cap; none are kept.
    TooMany,
}

/// What a conflict-free merge would write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The merge is clean and writes this tree (its object id).
    Clean(String),
    /// The merge conflicts.
    Conflicted,
}

impl ObjectRepo {
    /// The commits reachable from `head` and not from `base`
    /// (`git rev-list --parents --max-count=<max + 1> base..head`), newest first.
    ///
    /// # Errors
    /// [`GitError`] when git cannot run or fails (a missing commit included), or prints a line
    /// that is not commit ids. More than `max` commits is [`RangeCommits::TooMany`], not an
    /// error; at most `max + 1` lines are read.
    pub fn range_commits(
        &self,
        base: &CommitSha,
        head: &CommitSha,
        max: usize,
    ) -> Result<RangeCommits, GitError> {
        let limit = format!("--max-count={}", max.saturating_add(1));
        let range = format!("{base}..{head}");
        let captured = self.run(self.call(GitOperation::RevListParents, &[&limit, &range]))?;
        let text = str::from_utf8(&captured.stdout).map_err(|_not_utf8| malformed())?;
        let commits = text
            .lines()
            .map(parse_line)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(malformed)?;
        if commits.len() > max {
            return Ok(RangeCommits::TooMany);
        }
        Ok(RangeCommits::Listed(commits))
    }

    /// The object id of `commit`'s tree (`git rev-parse --verify <commit>^{tree}`).
    ///
    /// # Errors
    /// [`GitError`] when git cannot run or fails (a missing commit included), or does not print
    /// one object id.
    pub fn tree_of(&self, commit: &CommitSha) -> Result<String, GitError> {
        let object = format!("{commit}^{{tree}}");
        let captured = self.run(self.call(GitOperation::RevParse, &["--verify", &object]))?;
        object_id(&captured.stdout).ok_or(GitError::Malformed {
            operation: GitOperation::RevParse,
            detail: "rev-parse did not print one tree id",
        })
    }

    /// What `git merge-tree --write-tree --no-messages ours theirs` writes: the merged tree
    /// when the merge is clean (exit 0), [`MergeOutcome::Conflicted`] when it conflicts
    /// (exit 1). The tree's objects are written to this repository, nothing else is.
    ///
    /// # Errors
    /// [`GitError`] when git cannot run or fails any other way, or a clean merge does not print
    /// one tree id.
    pub fn merge_tree(
        &self,
        ours: &CommitSha,
        theirs: &CommitSha,
    ) -> Result<MergeOutcome, GitError> {
        let args = [ours.as_str(), theirs.as_str()];
        match self.run(self.call(GitOperation::MergeTree, &args)) {
            Ok(captured) => {
                object_id(&captured.stdout)
                    .map(MergeOutcome::Clean)
                    .ok_or(GitError::Malformed {
                        operation: GitOperation::MergeTree,
                        detail: "merge-tree did not print one tree id",
                    })
            }
            Err(GitError::Failed { status, .. }) if status.code() == Some(1) => {
                Ok(MergeOutcome::Conflicted)
            }
            Err(error) => Err(error),
        }
    }
}

/// `<commit> <parent>...` as a [`RangeCommit`].
fn parse_line(line: &str) -> Option<RangeCommit> {
    let mut ids = line.split(' ').map(str::parse::<CommitSha>);
    let commit = ids.next()?.ok()?;
    let parents = ids.collect::<Result<Vec<_>, _>>().ok()?;
    Some(RangeCommit { commit, parents })
}

/// One object id and a newline, as rev-parse and merge-tree print it.
fn object_id(stdout: &[u8]) -> Option<String> {
    str::from_utf8(stdout)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .filter(|id| tree::is_object_id(id))
        .map(str::to_owned)
}

/// The refusal of a rev-list listing that is not lines of commit ids.
const fn malformed() -> GitError {
    GitError::Malformed {
        operation: GitOperation::RevListParents,
        detail: "rev-list --parents did not print lines of commit ids",
    }
}

#[cfg(test)]
mod tests {
    use super::{object_id, parse_line};

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn a_line_is_a_commit_and_its_parents() {
        let line = format!("{A} {B} {B}");

        let parsed = parse_line(&line).map(|commit| (commit.commit, commit.parents.len()));

        assert_eq!(
            parsed.map(|(commit, parents)| (commit.to_string(), parents)),
            Some((A.to_owned(), 2))
        );
    }

    #[test]
    fn a_line_with_anything_but_commit_ids_is_refused() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line(&format!("{A}  {B}")), None);
        assert_eq!(parse_line(&format!("{A} HEAD")), None);
    }

    #[test]
    fn an_object_id_needs_exactly_one_line() {
        assert_eq!(object_id(format!("{A}\n").as_bytes()), Some(A.to_owned()));
        assert_eq!(object_id(A.as_bytes()), None);
        assert_eq!(object_id(format!("{A}\n{B}\n").as_bytes()), None);
    }
}
