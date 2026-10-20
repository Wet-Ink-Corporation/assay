//! What doctor's repository checks found: one result per [`Prerequisite`], in report order.

use std::fmt;

use thiserror::Error;

use super::port::{Access, BypassActor, RulesetId};
use super::workflow::{Holding, WorkflowError};
use crate::config::ConfigError;
use crate::publish::CheckName;
use crate::status::UserId;

/// One repository prerequisite of 05 §7, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prerequisite {
    /// Each protected base requires its verdict check from the App, up to date, with no bypass
    /// (R3-03, R3-01).
    VerdictRuleset,
    /// Every other protected base pins the default branch's `[assay].digest` (R3-27).
    BaseDigest,
    /// No installed assay workflow runs from a PR merge commit (T20).
    MergeCommitTrigger,
    /// Pushes to the trust paths are restricted.
    PathRestriction,
    /// No agent is an owner or approver, and no maintainer is either (R3-01).
    IdentityLists,
    /// An approver exists and is not an agent (H-13, R3-01).
    Approver,
    /// No agent may administer the repository (no ruleset or bypass rights).
    AgentAccess,
    /// Actions may not create or approve PRs.
    ActionsApproval,
    /// Every action and reusable workflow is pinned by full commit SHA.
    PinnedActions,
    /// `assay-publish` and `assay-review` deploy from the default branch only (T7, R3-04).
    Environments,
    /// Every installed job holding a secret or a write token carries the guard (T22, R3-05).
    RunAttemptGuard,
    /// The ledger's collaborators are owners only (T23, R3-20).
    LedgerCollaborators,
    /// A ruleset restricts every ledger branch to the ledger App (T23, R3-20).
    LedgerRuleset,
    /// No agent has access to the ledger (T23, R3-20).
    LedgerAgents,
}

impl Prerequisite {
    /// Every prerequisite, in report order.
    pub const ALL: [Self; 14] = [
        Self::VerdictRuleset,
        Self::BaseDigest,
        Self::MergeCommitTrigger,
        Self::PathRestriction,
        Self::IdentityLists,
        Self::Approver,
        Self::AgentAccess,
        Self::ActionsApproval,
        Self::PinnedActions,
        Self::Environments,
        Self::RunAttemptGuard,
        Self::LedgerCollaborators,
        Self::LedgerRuleset,
        Self::LedgerAgents,
    ];
}

impl fmt::Display for Prerequisite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::VerdictRuleset => "verdict-ruleset",
            Self::BaseDigest => "base-digest",
            Self::MergeCommitTrigger => "merge-commit-trigger",
            Self::PathRestriction => "path-restriction",
            Self::IdentityLists => "identity-lists",
            Self::Approver => "approver",
            Self::AgentAccess => "agent-access",
            Self::ActionsApproval => "actions-approval",
            Self::PinnedActions => "pinned-actions",
            Self::Environments => "environments",
            Self::RunAttemptGuard => "run-attempt-guard",
            Self::LedgerCollaborators => "ledger-collaborators",
            Self::LedgerRuleset => "ledger-ruleset",
            Self::LedgerAgents => "ledger-agents",
        })
    }
}

/// Why a prerequisite failed. Names and paths come from GitHub and are shown debug-quoted.
#[derive(Debug, Error)]
pub enum PrerequisiteFailure<E> {
    /// What the check reads could not be read, so it is not shown to hold.
    #[error("GitHub could not be read")]
    Unreadable(#[source] E),
    /// A read several checks share failed; its cause is reported under the first of them.
    #[error(
        "{what} could not be read (the cause is reported under the first check that reads them)"
    )]
    AlreadyReported {
        /// What could not be read.
        what: &'static str,
    },
    /// The default branch is not in `protected_bases`, so no verdict is published for it.
    #[error("the default branch {branch:?} is not in protected_bases")]
    DefaultNotProtected {
        /// The default branch.
        branch: String,
    },
    /// No active rule on `base` requires the verdict check from the App.
    #[error(
        "no active ruleset on {base:?} requires {:?} from the assay-verdict App",
        check.as_str()
    )]
    CheckNotRequired {
        /// The base.
        base: String,
        /// The check it must require.
        check: CheckName,
    },
    /// The rule requiring the verdict does not require branches to be up to date.
    #[error("ruleset {ruleset} requires the verdict on {base:?} without branches up to date")]
    NotUpToDate {
        /// The base.
        base: String,
        /// The ruleset.
        ruleset: RulesetId,
    },
    /// A ruleset doctor relies on lists a bypass actor; only an empty list is accepted.
    #[error("ruleset {ruleset} can be bypassed by {actor}")]
    Bypass {
        /// The ruleset.
        ruleset: RulesetId,
        /// Its first bypass actor.
        actor: BypassActor,
    },
    /// The ruleset requiring the verdict is not actively enforced.
    #[error("ruleset {ruleset} is not active")]
    NotActive {
        /// The ruleset.
        ruleset: RulesetId,
    },
    /// A rule in force names a ruleset the repository's list does not hold.
    #[error("ruleset {ruleset} is not among the repository's rulesets")]
    UnknownRuleset {
        /// The ruleset.
        ruleset: RulesetId,
    },
    /// A protected base has no `.assay/plugins.toml`.
    #[error("{base:?} has no .assay/plugins.toml")]
    BaseConfigMissing {
        /// The base.
        base: String,
    },
    /// A protected base's `.assay/plugins.toml` is not UTF-8.
    #[error("{base:?}'s .assay/plugins.toml is not UTF-8")]
    BaseConfigNotUtf8 {
        /// The base.
        base: String,
    },
    /// A protected base's `.assay/plugins.toml` was refused.
    #[error("{base:?}'s .assay/plugins.toml was refused")]
    BaseConfig {
        /// The base.
        base: String,
        /// Why.
        #[source]
        source: ConfigError,
    },
    /// A protected base pins another assay digest than the default branch.
    #[error("{base:?} pins another [assay].digest than the default branch")]
    DigestDiffers {
        /// The base.
        base: String,
    },
    /// No `assay*.yml` workflow is installed.
    #[error("no assay workflow is installed in .github/workflows")]
    NoWorkflows,
    /// A workflow entry is not a regular file.
    #[error("{path:?} is not a regular file")]
    WorkflowNotAFile {
        /// Its path.
        path: String,
    },
    /// A workflow file is not UTF-8.
    #[error("{path:?} is not UTF-8")]
    WorkflowNotUtf8 {
        /// Its path.
        path: String,
    },
    /// A listed workflow file could not be fetched.
    #[error("{path:?} is listed but could not be fetched")]
    WorkflowVanished {
        /// Its path.
        path: String,
    },
    /// A workflow file could not be classified.
    #[error("{path:?}")]
    Workflow {
        /// Its path.
        path: String,
        /// Why.
        #[source]
        source: WorkflowError,
    },
    /// An installed workflow is triggered by an event outside the allowlist of events that
    /// run the default branch's workflow file (T20).
    #[error(
        "{path:?} is triggered by {event:?}, which is not an event that runs the default branch's workflow file"
    )]
    MergeCommitTrigger {
        /// Its path.
        path: String,
        /// The event.
        event: String,
    },
    /// An action or reusable workflow is not pinned by full commit SHA.
    #[error("{path:?} uses {uses:?}, which is not pinned by full commit SHA")]
    Unpinned {
        /// The workflow.
        path: String,
        /// The `uses` value.
        uses: String,
    },
    /// A privileged job has no `github.run_attempt == 1` guard.
    #[error("job {job:?} of {path:?} holds {holds} without the github.run_attempt == 1 guard")]
    Unguarded {
        /// The workflow.
        path: String,
        /// The job.
        job: String,
        /// What it holds.
        holds: Holding,
    },
    /// A trust path is not restricted by an active push ruleset without bypass.
    #[error("no active push ruleset restricts {path:?}")]
    PathUnrestricted {
        /// The path pattern.
        path: &'static str,
    },
    /// An agent is in `owners` or `approvers`.
    #[error("agent {} is in owners or approvers", user.0)]
    AgentPrivileged {
        /// The agent.
        user: UserId,
    },
    /// A maintainer is in `owners` or `approvers`.
    #[error("maintainer {} is also in owners or approvers", user.0)]
    MaintainerPrivileged {
        /// The maintainer.
        user: UserId,
    },
    /// `approvers` is empty: no PR has an eligible approver.
    #[error("no approver is listed in .assay/owners (H-13)")]
    NoApprover,
    /// An approver is an agent.
    #[error("approver {} is an agent", user.0)]
    ApproverIsAgent {
        /// The approver.
        user: UserId,
    },
    /// An agent has more than `write` access.
    #[error("agent {} has {access} access", user.0)]
    AgentAccess {
        /// The agent.
        user: UserId,
        /// Their access.
        access: Access,
    },
    /// Actions may create and approve PRs.
    #[error("GitHub Actions may create and approve pull requests")]
    ActionsCanApprove,
    /// An environment does not exist.
    #[error("environment {name:?} does not exist")]
    EnvironmentMissing {
        /// The environment.
        name: &'static str,
    },
    /// An environment deploys from more than the default branch.
    #[error("environment {name:?} does not restrict deployments to the default branch only")]
    DeploymentBranches {
        /// The environment.
        name: &'static str,
    },
    /// `[assay].ledger_repo` is not set.
    #[error("no ledger_repo is configured")]
    NoLedger,
    /// A ledger collaborator is not an owner.
    #[error("ledger collaborator {} is not in owners", user.0)]
    LedgerCollaborator {
        /// The collaborator.
        user: UserId,
    },
    /// No active ruleset restricts every ledger branch to the ledger App.
    #[error("no active ruleset restricts refs/heads/** on the ledger to the ledger App")]
    LedgerUnrestricted,
    /// An agent has access to the ledger.
    #[error("agent {} has {access} access to the ledger", user.0)]
    LedgerAgentAccess {
        /// The agent.
        user: UserId,
        /// Their access.
        access: Access,
    },
}

/// One prerequisite and, when it failed, why.
#[derive(Debug)]
pub struct PrerequisiteResult<E> {
    /// The prerequisite.
    pub prerequisite: Prerequisite,
    /// Why it failed; `None` when it holds.
    pub failure: Option<PrerequisiteFailure<E>>,
}

/// The result of every prerequisite, in [`Prerequisite::ALL`] order.
#[derive(Debug)]
pub struct PrerequisiteReport<E> {
    pub(super) results: Vec<PrerequisiteResult<E>>,
}

impl<E> PrerequisiteReport<E> {
    /// Each prerequisite's result, in report order.
    #[must_use]
    pub fn results(&self) -> &[PrerequisiteResult<E>] {
        &self.results
    }

    /// Whether every prerequisite holds.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.results.iter().all(|result| result.failure.is_none())
    }
}
