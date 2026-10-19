//! The repository prerequisites `assay doctor` enforces (05 §7, #75), read through the
//! [`PrerequisiteForge`] port: the verdict rulesets of every protected base (R3-03, R3-01), the
//! same assay digest on every protected base (R3-27), no PR-merge-commit trigger (T20),
//! restricted trust paths, the identity lists (R3-01), no agent administering the repository,
//! Actions unable to approve, SHA-pinned actions, the two environments deploying from the
//! default branch only (T7, R3-04), the `run_attempt == 1` guard (T22, R3-05) and the ledger
//! repository's access (T23, R3-20).
//!
//! [`read_base`] reads base config from the default branch; a failure there means nothing can
//! be judged. [`prerequisites`] then judges each [`Prerequisite`], failing it closed when what
//! it reads cannot be read or classified (wi-b5affa). The token expiry warning is the session
//! doctor's (`token_expiry_warning`).

mod access;
mod identities;
mod port;
mod report;
mod rulesets;
mod shared;
mod workflow;
mod workflows;
mod yaml;

use shared::Shared;
use workflows::WorkflowFile;

use thiserror::Error;

pub use identities::{IdentityListError, IdentityLists, MAX_LISTED};
pub use port::{
    Access, AppliedRule, BranchPolicy, BypassActor, Collaborator, Conditions, DeploymentBranches,
    DirectoryEntry, Enforcement, EntryType, PolicyKind, PrerequisiteForge, RequiredCheck,
    RequiredChecks, Rule, Ruleset, RulesetId, RulesetTarget, WorkflowPermissions,
};
pub use report::{Prerequisite, PrerequisiteFailure, PrerequisiteReport, PrerequisiteResult};
pub use workflow::{Holding, WorkflowError};
pub use yaml::{YamlError, YamlRefusal};

use crate::config::{ConfigError, PluginsConfig};
use crate::status::AppId;

/// Base `.assay/plugins.toml`.
pub const PLUGINS_PATH: &str = ".assay/plugins.toml";
/// Base `.assay/owners`.
pub const OWNERS_PATH: &str = ".assay/owners";
/// Base `.assay/agents`.
pub const AGENTS_PATH: &str = ".assay/agents";

/// Base config as the default branch holds it: what every check is judged against.
#[derive(Debug)]
pub struct PrerequisiteBase {
    default_branch: String,
    config: PluginsConfig,
    identities: IdentityLists,
}

impl PrerequisiteBase {
    /// The default branch.
    #[must_use]
    pub fn default_branch(&self) -> &str {
        &self.default_branch
    }

    /// Its `.assay/plugins.toml`.
    #[must_use]
    pub const fn config(&self) -> &PluginsConfig {
        &self.config
    }

    /// Its `.assay/owners` and `.assay/agents`.
    #[must_use]
    pub const fn identities(&self) -> &IdentityLists {
        &self.identities
    }
}

/// The two GitHub Apps the checks name, by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrerequisiteApps {
    /// `assay-verdict`: the only source of the required verdict check (T7).
    pub verdict: AppId,
    /// `assay-ledger-writer`: the only writer of the ledger (ADR-26).
    pub ledger: AppId,
}

/// Why base config could not be read, so no prerequisite can be judged.
#[derive(Debug, Error)]
pub enum BaseError<E> {
    /// GitHub could not be read.
    #[error("GitHub could not be read")]
    Forge(#[source] E),
    /// A base file is missing.
    #[error("the default branch {branch:?} has no {path}")]
    Missing {
        /// The file.
        path: &'static str,
        /// The default branch.
        branch: String,
    },
    /// A base file is not UTF-8.
    #[error("{path} is not UTF-8")]
    NotUtf8 {
        /// The file.
        path: &'static str,
    },
    /// `.assay/plugins.toml` was refused.
    #[error("{PLUGINS_PATH} was refused")]
    Config(#[source] ConfigError),
    /// `.assay/owners` or `.assay/agents` was refused.
    #[error("{OWNERS_PATH} or {AGENTS_PATH} was refused")]
    Identities(#[source] IdentityListError),
}

/// Reads the default branch's `.assay/plugins.toml`, `.assay/owners` and `.assay/agents`.
///
/// # Errors
///
/// [`BaseError`] when GitHub cannot be read, a file is missing or not UTF-8, or a file is
/// refused.
pub fn read_base<F: PrerequisiteForge>(forge: &F) -> Result<PrerequisiteBase, BaseError<F::Error>> {
    let default_branch = forge.default_branch().map_err(BaseError::Forge)?;
    let config = base_text(forge, &default_branch, PLUGINS_PATH)?
        .parse()
        .map_err(BaseError::Config)?;
    let owners = base_text(forge, &default_branch, OWNERS_PATH)?;
    let agents = base_text(forge, &default_branch, AGENTS_PATH)?;
    let identities = IdentityLists::parse(&owners, &agents).map_err(BaseError::Identities)?;
    Ok(PrerequisiteBase {
        default_branch,
        config,
        identities,
    })
}

/// The text of `path` on `branch`.
fn base_text<F: PrerequisiteForge>(
    forge: &F,
    branch: &str,
    path: &'static str,
) -> Result<String, BaseError<F::Error>> {
    let bytes = forge
        .file(branch, path)
        .map_err(BaseError::Forge)?
        .ok_or_else(|| BaseError::Missing {
            path,
            branch: branch.to_owned(),
        })?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(_) => Err(BaseError::NotUtf8 { path }),
    }
}

/// What every check reads from. The bound on the struct names the forge's error type, which
/// the shared reads keep.
struct Scene<'a, F: PrerequisiteForge> {
    repository: &'a F,
    ledger: Option<&'a F>,
    base: &'a PrerequisiteBase,
    apps: PrerequisiteApps,
    /// The repository's rulesets, read once for the verdict and path checks.
    rulesets: Shared<Vec<Ruleset>, F::Error>,
    /// The default branch's workflow files, read once for the three workflow checks.
    workflows: Shared<Vec<WorkflowFile>, F::Error>,
}

/// One check's outcome: `Err` with the failure.
type Judged<E> = Result<(), PrerequisiteFailure<E>>;

/// Judges every [`Prerequisite`] of `repository` against `base`.
///
/// `ledger` is the ledger repository; `None` (no `[assay].ledger_repo`) fails the ledger
/// checks. A check whose reads fail is failed with [`PrerequisiteFailure::Unreadable`]; the
/// others are still judged. The rulesets and the workflow files are read once each: when
/// such a read fails, the first check that needs it reports the cause and the later ones
/// [`PrerequisiteFailure::AlreadyReported`].
pub fn prerequisites<F: PrerequisiteForge>(
    repository: &F,
    ledger: Option<&F>,
    base: &PrerequisiteBase,
    apps: PrerequisiteApps,
) -> PrerequisiteReport<F::Error> {
    let scene = Scene {
        repository,
        ledger,
        base,
        apps,
        rulesets: Shared::new("the rulesets", repository.rulesets()),
        workflows: Shared::new(
            "the workflow files",
            workflows::read_files(repository, &base.default_branch),
        ),
    };
    let results = Prerequisite::ALL
        .into_iter()
        .map(|prerequisite| PrerequisiteResult {
            prerequisite,
            failure: judge(prerequisite, &scene).err(),
        })
        .collect();
    PrerequisiteReport { results }
}

fn judge<F: PrerequisiteForge>(
    prerequisite: Prerequisite,
    scene: &Scene<'_, F>,
) -> Judged<F::Error> {
    match prerequisite {
        Prerequisite::VerdictRuleset => rulesets::verdict(scene),
        Prerequisite::BaseDigest => access::base_digest(scene),
        Prerequisite::MergeCommitTrigger => workflows::merge_commit_trigger(scene),
        Prerequisite::PathRestriction => rulesets::paths(scene),
        Prerequisite::IdentityLists => access::identity_lists(scene),
        Prerequisite::Approver => access::approver(scene),
        Prerequisite::AgentAccess => access::agent_access(scene),
        Prerequisite::ActionsApproval => access::actions_approval(scene),
        Prerequisite::PinnedActions => workflows::pinned_actions(scene),
        Prerequisite::Environments => access::environments(scene),
        Prerequisite::RunAttemptGuard => workflows::run_attempt_guard(scene),
        Prerequisite::LedgerCollaborators => access::ledger_collaborators(scene),
        Prerequisite::LedgerRuleset => rulesets::ledger(scene),
        Prerequisite::LedgerAgents => access::ledger_agents(scene),
    }
}
