//! The pinned base and head commits of `gates-static` and `gates-lint`, fetched into a hardened
//! object-only repository in the job's scratch directory (03 §9).
//!
//! The commits come from `ASSAY_BASE_SHA` and `ASSAY_HEAD_SHA` (guard's outputs) and are fetched
//! by SHA from `https://github.com/$GITHUB_REPOSITORY.git`, with an authorized git fetch whose
//! header is `ASSAY_RUN_TOKEN`'s (the job's `github.token`) and no other token's. The token
//! reaches no gate: the gate environment's allowlist drops it, and git inherits only `PATH`.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use assay_forge::git::{
    Authorization, CommitSha, GitEnvironment, GitLimits, HttpsRemote, ObjectRepo,
};
use assay_forge::github::{GhToken, Repository};
use assay_forge::process::GroupKiller;

use super::{PinnedError, Side, Variable};

/// What the composition root read of the process for the fetch, and `gates-lint`'s merge base
/// (ARC-11). Its `Debug` shows whether the token is set, never its value.
pub(crate) struct PinnedVariables {
    /// [`Variable::BaseSha`].
    pub(crate) base_sha: Option<OsString>,
    /// [`Variable::HeadSha`].
    pub(crate) head_sha: Option<OsString>,
    /// [`Variable::MergeBase`], which only `gates-lint` reads.
    pub(crate) merge_base: Option<OsString>,
    /// [`Variable::RunToken`].
    pub(crate) token: Option<OsString>,
    /// [`Variable::Repository`].
    pub(crate) repository: Option<OsString>,
    /// [`Variable::Path`].
    pub(crate) path: Option<OsString>,
}

impl fmt::Debug for PinnedVariables {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedVariables")
            .field("base_sha", &self.base_sha)
            .field("head_sha", &self.head_sha)
            .field("merge_base", &self.merge_base)
            .field("token", &self.token.as_ref().map(|_set| "<redacted>"))
            .field("repository", &self.repository)
            .field("path", &self.path)
            .finish()
    }
}

/// Where the pinned commits are fetched from, and how.
pub(super) struct Source {
    base: CommitSha,
    head: CommitSha,
    remote: HttpsRemote,
    /// The run token as git's header, sent to `remote` only.
    authorization: Authorization,
    /// `PATH`, the only variable git inherits.
    path: OsString,
}

impl Source {
    /// Reads the fetch's variables, each refused by name and never shown; the merge base is
    /// not read here.
    pub(super) fn read(variables: PinnedVariables) -> Result<Self, PinnedError> {
        let base = commit(variables.base_sha, Variable::BaseSha)?;
        let head = commit(variables.head_sha, Variable::HeadSha)?;
        let authorization = present(variables.token, Variable::RunToken)?
            .to_str()
            .and_then(|text| GhToken::new(text).ok())
            .ok_or(PinnedError::Invalid(Variable::RunToken))?
            .git_authorization()
            .map_err(PinnedError::Authorization)?;
        let repo: Repository = present(variables.repository, Variable::Repository)?
            .to_str()
            .and_then(|text| text.parse().ok())
            .ok_or(PinnedError::Invalid(Variable::Repository))?;
        let remote = format!("https://github.com/{repo}.git")
            .parse()
            .map_err(PinnedError::Remote)?;
        let path = present(variables.path, Variable::Path)?;
        Ok(Self {
            base,
            head,
            remote,
            authorization,
            path,
        })
    }

    /// The pinned base commit.
    pub(super) const fn base(&self) -> &CommitSha {
        &self.base
    }

    /// Fetches the base and then the head commit, by SHA, into an object-only repository at
    /// `git_dir`, whose git processes `killer` stops.
    pub(super) fn fetch(
        self,
        git_dir: PathBuf,
        killer: GroupKiller,
    ) -> Result<Fetched, PinnedError> {
        let git = GitEnvironment::new(self.path, killer);
        let repo =
            ObjectRepo::init(git_dir, git, GitLimits::default()).map_err(PinnedError::Git)?;
        for (side, commit) in [(Side::Base, &self.base), (Side::Head, &self.head)] {
            repo.fetch_authorized(&self.remote, &self.authorization, commit)
                .map_err(|source| PinnedError::Fetch { side, source })?;
        }
        Ok(Fetched {
            repo,
            base: self.base,
            head: self.head,
        })
    }
}

/// The object-only repository holding both pinned commits.
pub(super) struct Fetched {
    pub(super) repo: ObjectRepo,
    pub(super) base: CommitSha,
    pub(super) head: CommitSha,
}

/// `value`, which must be set.
fn present(value: Option<OsString>, variable: Variable) -> Result<OsString, PinnedError> {
    value.ok_or(PinnedError::Missing(variable))
}

/// The commit `value` names: 40 lowercase hex digits, as a [`CommitSha`] or the app's
/// `CommitId`.
pub(super) fn commit<C: FromStr>(
    value: Option<OsString>,
    variable: Variable,
) -> Result<C, PinnedError> {
    present(value, variable)?
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or(PinnedError::Invalid(variable))
}
