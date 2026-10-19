//! The `PrerequisiteForge` port: the Forge reads doctor's repository checks need (05 §7).
//! Implemented by `assay-forge`'s `GitHub`, once for the repository and once for the ledger
//! repository.

use std::fmt;
use std::num::NonZeroU64;

use crate::status::{AppId, UserId};

/// GitHub's settings of one repository, read-only. Every text it returns is untrusted.
pub trait PrerequisiteForge {
    /// Why GitHub could not be read.
    type Error: std::error::Error;

    /// The repository's default branch.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the repository cannot be read.
    fn default_branch(&self) -> Result<String, Self::Error>;

    /// The bytes of the file at `path` on `branch`; `None` when there is none.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the file cannot be read, or `path` is not a file.
    fn file(&self, branch: &str, path: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// The entries of the directory at `path` on `branch`; `None` when there is none.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the directory cannot be listed, or `path` is not a directory.
    fn directory(
        &self,
        branch: &str,
        path: &str,
    ) -> Result<Option<Vec<DirectoryEntry>>, Self::Error>;

    /// Every ruleset of the repository, its organization's included, each read in full.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when a ruleset cannot be listed or read.
    fn rulesets(&self) -> Result<Vec<Ruleset>, Self::Error>;

    /// The rules of active rulesets that apply to `branch`, as GitHub evaluates their
    /// conditions.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the rules cannot be read.
    fn branch_rules(&self, branch: &str) -> Result<Vec<AppliedRule>, Self::Error>;

    /// Which branches may deploy to environment `name`; `None` when there is no such
    /// environment.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the environment or its branch policies cannot be read.
    fn environment(&self, name: &str) -> Result<Option<DeploymentBranches>, Self::Error>;

    /// What the repository lets the `GITHUB_TOKEN` of its workflows do.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the settings cannot be read.
    fn workflow_permissions(&self) -> Result<WorkflowPermissions, Self::Error>;

    /// Every user with access to the repository, through the organization included.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the collaborators cannot be listed.
    fn collaborators(&self) -> Result<Vec<Collaborator>, Self::Error>;

    /// What `user` may do in the repository.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the user or their permission cannot be read.
    fn access(&self, user: UserId) -> Result<Access, Self::Error>;
}

/// One entry of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Its file name.
    pub name: String,
    /// What it is.
    pub kind: EntryType,
}

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryType {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Symlink,
    /// A submodule.
    Submodule,
}

/// A ruleset's numeric id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RulesetId(pub NonZeroU64);

impl fmt::Display for RulesetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One ruleset, as `GET /repos/{owner}/{repo}/rulesets/{id}` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ruleset {
    /// Its id.
    pub id: RulesetId,
    /// Its name, for display.
    pub name: String,
    /// What it applies to.
    pub target: RulesetTarget,
    /// Whether it is enforced.
    pub enforcement: Enforcement,
    /// The refs it applies to.
    pub conditions: Conditions,
    /// Who may bypass it.
    pub bypass: Vec<BypassActor>,
    /// Its rules.
    pub rules: Vec<Rule>,
}

/// What a ruleset applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesetTarget {
    /// Branches.
    Branch,
    /// Tags.
    Tag,
    /// Pushes to the repository and its forks.
    Push,
    /// Any other target, by name.
    Other(String),
}

/// A ruleset's enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Enforcement {
    /// Enforced.
    Active,
    /// Only evaluated: nothing is blocked.
    Evaluate,
    /// Off.
    Disabled,
}

/// A ruleset's conditions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conditions {
    /// `ref_name.include`: the ref patterns it applies to.
    pub included: Vec<String>,
    /// `ref_name.exclude`: the ref patterns it does not apply to.
    pub excluded: Vec<String>,
    /// Whether any condition other than `ref_name` is set (an organization ruleset's
    /// repository conditions, for instance).
    pub other: bool,
}

/// Who may bypass a ruleset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BypassActor {
    /// A GitHub App, by its id.
    Integration(AppId),
    /// A team, by its id.
    Team(NonZeroU64),
    /// Every organization owner.
    OrganizationAdmin,
    /// Everyone with a repository role, by the role's id.
    RepositoryRole(NonZeroU64),
    /// A deploy key.
    DeployKey,
    /// Any other actor type, by name.
    Other(String),
}

impl fmt::Display for BypassActor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integration(app) => write!(f, "app {}", app.0),
            Self::Team(team) => write!(f, "team {team}"),
            Self::OrganizationAdmin => f.write_str("organization owners"),
            Self::RepositoryRole(role) => write!(f, "repository role {role}"),
            Self::DeployKey => f.write_str("deploy keys"),
            Self::Other(kind) => write!(f, "actor type {kind:?}"),
        }
    }
}

/// One rule of a ruleset. Rules doctor does not read are kept by type only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `required_status_checks`.
    RequiredStatusChecks(RequiredChecks),
    /// `file_path_restriction`, with its restricted paths.
    FilePathRestriction(Vec<String>),
    /// `creation`: only bypass actors may create matching refs.
    Creation,
    /// `update`: only bypass actors may update matching refs.
    Update,
    /// `deletion`: only bypass actors may delete matching refs.
    Deletion,
    /// Any other rule, by type.
    Other(String),
}

/// The parameters of a `required_status_checks` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredChecks {
    /// The required checks.
    pub checks: Vec<RequiredCheck>,
    /// `strict_required_status_checks_policy`: branches must be up to date.
    pub strict: bool,
}

/// One required check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredCheck {
    /// Its name.
    pub context: String,
    /// The app it must come from; `None` for any source.
    pub integration: Option<AppId>,
}

/// A rule in force on a branch, with the ruleset it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedRule {
    /// The ruleset.
    pub ruleset: RulesetId,
    /// The rule.
    pub rule: Rule,
}

/// Which branches may deploy to an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentBranches {
    /// Every branch (no deployment branch policy).
    All,
    /// Protected branches.
    Protected,
    /// The listed branch and tag patterns.
    Custom(Vec<BranchPolicy>),
}

/// One deployment branch or tag policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPolicy {
    /// Its name pattern.
    pub name: String,
    /// Whether it matches branches or tags.
    pub kind: PolicyKind,
}

/// What a deployment policy matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PolicyKind {
    /// Branches.
    Branch,
    /// Tags.
    Tag,
}

/// What a repository lets its workflows' `GITHUB_TOKEN` do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkflowPermissions {
    /// `can_approve_pull_request_reviews`: Actions may create and approve PRs.
    pub can_approve_pull_requests: bool,
}

/// A user with access to a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collaborator {
    /// The user.
    pub user: UserId,
    /// Their access.
    pub access: Access,
}

/// A user's access to a repository, by GitHub's role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// `admin`.
    Admin,
    /// `maintain`.
    Maintain,
    /// `write`.
    Write,
    /// `triage`.
    Triage,
    /// `read`.
    Read,
    /// No access.
    None,
    /// A custom role, by name.
    Custom(String),
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admin => f.write_str("admin"),
            Self::Maintain => f.write_str("maintain"),
            Self::Write => f.write_str("write"),
            Self::Triage => f.write_str("triage"),
            Self::Read => f.write_str("read"),
            Self::None => f.write_str("none"),
            Self::Custom(role) => write!(f, "custom role {role:?}"),
        }
    }
}
