//! Who may do what: the identity lists (R3-01, H-13), agent and Actions rights, the two
//! environments (T7, R3-04), the protected bases' assay digest (R3-27) and the ledger's access
//! (T23, R3-20).

use std::collections::BTreeSet;

use super::port::{Access, BranchPolicy, DeploymentBranches, PolicyKind, PrerequisiteForge};
use super::report::PrerequisiteFailure;
use super::{Judged, PLUGINS_PATH, Scene};
use crate::config::PluginsConfig;
use crate::status::UserId;

/// The environments that hold assay's secrets.
pub(super) const ENVIRONMENTS: [&str; 2] = ["assay-publish", "assay-review"];

/// `owners ∪ approvers`: the privileged identities.
fn privileged<F>(scene: &Scene<'_, F>) -> BTreeSet<UserId> {
    let lists = &scene.base.identities;
    lists.owners.union(&lists.approvers).copied().collect()
}

/// No agent and no maintainer is an owner or approver.
pub(super) fn identity_lists<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let privileged = privileged(scene);
    let lists = &scene.base.identities;
    if let Some(&user) = lists.agents.intersection(&privileged).next() {
        return Err(PrerequisiteFailure::AgentPrivileged { user });
    }
    match lists.maintainers.intersection(&privileged).next() {
        Some(&user) => Err(PrerequisiteFailure::MaintainerPrivileged { user }),
        None => Ok(()),
    }
}

/// An approver is listed and none is an agent.
pub(super) fn approver<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let lists = &scene.base.identities;
    if lists.approvers.is_empty() {
        return Err(PrerequisiteFailure::NoApprover);
    }
    match lists.approvers.intersection(&lists.agents).next() {
        Some(&user) => Err(PrerequisiteFailure::ApproverIsAgent { user }),
        None => Ok(()),
    }
}

/// Whether `access` lets its holder change settings or rulesets.
fn administers(access: &Access) -> bool {
    match access {
        Access::Admin | Access::Maintain | Access::Custom(_) => true,
        Access::Write | Access::Triage | Access::Read | Access::None => false,
    }
}

/// No agent has more than `write` access (a custom role is not shown to have less).
pub(super) fn agent_access<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    for &user in &scene.base.identities.agents {
        let access = scene
            .repository
            .access(user)
            .map_err(PrerequisiteFailure::Unreadable)?;
        if administers(&access) {
            return Err(PrerequisiteFailure::AgentAccess { user, access });
        }
    }
    Ok(())
}

/// Actions may not create or approve PRs.
pub(super) fn actions_approval<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let permissions = scene
        .repository
        .workflow_permissions()
        .map_err(PrerequisiteFailure::Unreadable)?;
    if permissions.can_approve_pull_requests {
        Err(PrerequisiteFailure::ActionsCanApprove)
    } else {
        Ok(())
    }
}

/// Both environments exist and deploy from exactly the default branch.
pub(super) fn environments<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let default = &scene.base.default_branch;
    for name in ENVIRONMENTS {
        let branches = scene
            .repository
            .environment(name)
            .map_err(PrerequisiteFailure::Unreadable)?
            .ok_or(PrerequisiteFailure::EnvironmentMissing { name })?;
        if !only_default(&branches, default) {
            return Err(PrerequisiteFailure::DeploymentBranches { name });
        }
    }
    Ok(())
}

/// Whether `branches` is exactly one branch policy, naming `default`.
fn only_default(branches: &DeploymentBranches, default: &str) -> bool {
    match branches {
        DeploymentBranches::Custom(policies) => matches!(
            policies.as_slice(),
            [BranchPolicy { name, kind: PolicyKind::Branch }] if name == default
        ),
        DeploymentBranches::All | DeploymentBranches::Protected => false,
    }
}

/// Every protected base other than the default branch pins the default branch's digest.
pub(super) fn base_digest<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let assay = scene.base.config.assay();
    let others = assay
        .protected_bases
        .iter()
        .filter(|base| **base != scene.base.default_branch);
    for base in others {
        let missing = || PrerequisiteFailure::BaseConfigMissing { base: base.clone() };
        let bytes = scene
            .repository
            .file(base, PLUGINS_PATH)
            .map_err(PrerequisiteFailure::Unreadable)?
            .ok_or_else(missing)?;
        // Each failure owns the base's name; the config keeps its own.
        let Ok(text) = String::from_utf8(bytes) else {
            return Err(PrerequisiteFailure::BaseConfigNotUtf8 { base: base.clone() });
        };
        let config: PluginsConfig =
            text.parse()
                .map_err(|source| PrerequisiteFailure::BaseConfig {
                    base: base.clone(),
                    source,
                })?;
        if config.assay().digest != assay.digest {
            return Err(PrerequisiteFailure::DigestDiffers { base: base.clone() });
        }
    }
    Ok(())
}

/// Every ledger collaborator is an owner.
pub(super) fn ledger_collaborators<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let ledger = scene.ledger.ok_or(PrerequisiteFailure::NoLedger)?;
    let collaborators = ledger
        .collaborators()
        .map_err(PrerequisiteFailure::Unreadable)?;
    let owners = &scene.base.identities.owners;
    match collaborators
        .iter()
        .find(|collaborator| !owners.contains(&collaborator.user))
    {
        Some(stranger) => Err(PrerequisiteFailure::LedgerCollaborator {
            user: stranger.user,
        }),
        None => Ok(()),
    }
}

/// No agent has any access to the ledger.
pub(super) fn ledger_agents<F: PrerequisiteForge>(scene: &Scene<'_, F>) -> Judged<F::Error> {
    let ledger = scene.ledger.ok_or(PrerequisiteFailure::NoLedger)?;
    for &user in &scene.base.identities.agents {
        let access = ledger
            .access(user)
            .map_err(PrerequisiteFailure::Unreadable)?;
        if access != Access::None {
            return Err(PrerequisiteFailure::LedgerAgentAccess { user, access });
        }
    }
    Ok(())
}
