//! `assay-app`'s `PrerequisiteForge` port (05 §7, #75): the repository, its contents, its
//! rulesets and the rules in force on a branch, its environments, the Actions token's rights,
//! its collaborators and a user's access. Every response is decoded into a DTO and converted;
//! an unknown enforcement, entry type, policy type or role name that would change a judgement
//! is refused, and a list GitHub may grow (rule types, bypass actor types, rulesets' targets)
//! keeps unknown entries by name so doctor fails them closed.

mod dto;

use std::num::NonZeroU64;

use assay_app::{
    Access, AppId, AppliedRule, BranchPolicy, BypassActor, Collaborator, Conditions,
    DeploymentBranches, DirectoryEntry, Enforcement, EntryType, PolicyKind, PrerequisiteForge,
    RequiredCheck, RequiredChecks, Rule, Ruleset, RulesetId, RulesetTarget, UserId,
    WorkflowPermissions,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use self::dto::{
    BranchPoliciesPage, BranchRuleDto, BypassActorDto, CollaboratorDto, EntryDto, EnvironmentDto,
    FileDto, LoginDto, PathRestrictionDto, PermissionDto, RepositoryDto, RequiredChecksDto,
    RuleDto, RulesetDto, RulesetSummaryDto, WorkflowPermissionsDto,
};
use super::api::{PER_PAGE, percent_encode};
use super::error::{ForgeError, Operation};
use super::transport::{Sleep, Transport};
use super::{GitHub, api_path, base64, dto as wire};

impl<T: Transport, S: Sleep> PrerequisiteForge for GitHub<T, S> {
    type Error = ForgeError<T::Error>;

    fn default_branch(&self) -> Result<String, Self::Error> {
        let operation = Operation::ReadRepository;
        let path = api_path(operation, format!("repos/{}", self.repository))?;
        let repository: RepositoryDto = self.api.get(operation, &path)?;
        if repository.default_branch.is_empty() {
            return Err(malformed(operation, "the repository has no default branch"));
        }
        Ok(repository.default_branch)
    }

    fn file(&self, branch: &str, path: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        let operation = Operation::ReadContents;
        let path = self.contents(operation, branch, path)?;
        let Some(file) = self.api.get_optional::<FileDto>(operation, &path)? else {
            return Ok(None);
        };
        if file.kind != "file" || file.encoding.as_deref() != Some("base64") {
            return Err(malformed(operation, "not a base64-encoded file"));
        }
        let bytes = file
            .content
            .as_deref()
            .and_then(base64::decode)
            .ok_or_else(|| malformed(operation, "the content is not strict base64"))?;
        if u64::try_from(bytes.len()).ok() != Some(file.size) {
            return Err(malformed(operation, "the content does not match its size"));
        }
        Ok(Some(bytes))
    }

    fn directory(
        &self,
        branch: &str,
        path: &str,
    ) -> Result<Option<Vec<DirectoryEntry>>, Self::Error> {
        let operation = Operation::ReadContents;
        let path = self.contents(operation, branch, path)?;
        let Some(entries) = self.api.get_optional::<Vec<EntryDto>>(operation, &path)? else {
            return Ok(None);
        };
        entries
            .into_iter()
            .map(|entry| {
                let kind = match entry.kind.as_str() {
                    "file" => EntryType::File,
                    "dir" => EntryType::Dir,
                    "symlink" => EntryType::Symlink,
                    "submodule" => EntryType::Submodule,
                    _ => return Err(malformed(operation, "unknown directory entry type")),
                };
                if entry.name.is_empty() || entry.name.contains('/') {
                    return Err(malformed(operation, "a directory entry name is not a name"));
                }
                Ok(DirectoryEntry {
                    name: entry.name,
                    kind,
                })
            })
            .collect::<Result<_, _>>()
            .map(Some)
    }

    fn rulesets(&self) -> Result<Vec<Ruleset>, Self::Error> {
        let operation = Operation::ListRulesets;
        let path = self.path(
            operation,
            &format!("rulesets?includes_parents=true&per_page={PER_PAGE}"),
        )?;
        let listed = self
            .api
            .list::<Vec<RulesetSummaryDto>>(operation, path, None)?;
        listed
            .into_iter()
            .map(|summary| {
                let operation = Operation::ReadRuleset;
                let id = wire::nonzero(operation, summary.id, "a ruleset id is zero")?;
                let path = self.path(operation, &format!("rulesets/{id}"))?;
                ruleset(operation, self.api.get(operation, &path)?)
            })
            .collect()
    }

    fn branch_rules(&self, branch: &str) -> Result<Vec<AppliedRule>, Self::Error> {
        let operation = Operation::ReadBranchRules;
        let path = self.path(
            operation,
            &format!(
                "rules/branches/{}?per_page={PER_PAGE}",
                percent_encode(nonempty(operation, branch)?)
            ),
        )?;
        self.api
            .list::<Vec<BranchRuleDto>>(operation, path, None)?
            .into_iter()
            .map(|applied| {
                Ok(AppliedRule {
                    ruleset: RulesetId(wire::nonzero(
                        operation,
                        applied.ruleset_id,
                        "a ruleset id is zero",
                    )?),
                    rule: rule(operation, applied.rule)?,
                })
            })
            .collect()
    }

    fn environment(&self, name: &str) -> Result<Option<DeploymentBranches>, Self::Error> {
        let operation = Operation::ReadEnvironment;
        let name = percent_encode(nonempty(operation, name)?);
        let path = self.path(operation, &format!("environments/{name}"))?;
        let Some(environment) = self.api.get_optional::<EnvironmentDto>(operation, &path)? else {
            return Ok(None);
        };
        let branches = match environment.deployment_branch_policy {
            None => DeploymentBranches::All,
            Some(policy) => match (policy.protected_branches, policy.custom_branch_policies) {
                (true, false) => DeploymentBranches::Protected,
                (false, true) => DeploymentBranches::Custom(self.branch_policies(&name)?),
                (true, true) | (false, false) => {
                    return Err(malformed(operation, "an unknown deployment branch policy"));
                }
            },
        };
        Ok(Some(branches))
    }

    fn workflow_permissions(&self) -> Result<WorkflowPermissions, Self::Error> {
        let operation = Operation::ReadWorkflowPermissions;
        let path = self.path(operation, "actions/permissions/workflow")?;
        let permissions: WorkflowPermissionsDto = self.api.get(operation, &path)?;
        Ok(WorkflowPermissions {
            can_approve_pull_requests: permissions.can_approve_pull_request_reviews,
        })
    }

    fn collaborators(&self) -> Result<Vec<Collaborator>, Self::Error> {
        let operation = Operation::ListCollaborators;
        let path = self.path(
            operation,
            &format!("collaborators?affiliation=all&per_page={PER_PAGE}"),
        )?;
        self.api
            .list::<Vec<CollaboratorDto>>(operation, path, None)?
            .into_iter()
            .map(|collaborator| {
                Ok(Collaborator {
                    user: UserId(wire::nonzero(
                        operation,
                        collaborator.id,
                        "a user id is zero",
                    )?),
                    access: role(operation, collaborator.role_name)?,
                })
            })
            .collect()
    }

    fn access(&self, user: UserId) -> Result<Access, Self::Error> {
        let login = self.login(user)?;
        let operation = Operation::ReadPermission;
        let path = self.path(
            operation,
            &format!("collaborators/{}/permission", percent_encode(&login)),
        )?;
        let permission: PermissionDto = self.api.get(operation, &path)?;
        if permission.user.map(|answered| answered.id) != Some(user.0.get()) {
            return Err(malformed(operation, "the permission is of another user"));
        }
        if permission.permission == "none" {
            return Ok(Access::None);
        }
        role(operation, permission.role_name)
    }
}

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// `contents/<path>?ref=<branch>`, each path segment encoded; a path that is empty,
    /// absolute or has an empty, `.` or `..` segment is refused unsent.
    pub(super) fn contents(
        &self,
        operation: Operation,
        branch: &str,
        path: &str,
    ) -> Result<super::ApiPath, ForgeError<T::Error>> {
        let plain = !path.is_empty()
            && path
                .split('/')
                .all(|segment| !matches!(segment, "" | "." | ".."));
        if !plain {
            return Err(ForgeError::InvalidRequest {
                operation,
                detail: "the path is not a plain repository path",
            });
        }
        let segments: Vec<String> = path.split('/').map(percent_encode).collect();
        let branch = percent_encode(nonempty(operation, branch)?);
        self.path(
            operation,
            &format!("contents/{}?ref={branch}", segments.join("/")),
        )
    }

    /// Environment `name`'s deployment branch policies; `name` is already encoded.
    fn branch_policies(&self, name: &str) -> Result<Vec<BranchPolicy>, ForgeError<T::Error>> {
        let operation = Operation::ListBranchPolicies;
        let path = self.path(
            operation,
            &format!("environments/{name}/deployment-branch-policies?per_page={PER_PAGE}"),
        )?;
        self.api
            .list::<BranchPoliciesPage>(operation, path, None)?
            .into_iter()
            .map(|policy| {
                let kind = match policy.kind.as_str() {
                    "branch" => PolicyKind::Branch,
                    "tag" => PolicyKind::Tag,
                    _ => return Err(malformed(operation, "unknown branch policy type")),
                };
                Ok(BranchPolicy {
                    name: policy.name,
                    kind,
                })
            })
            .collect()
    }

    /// The login of `user`, checked against its id.
    fn login(&self, user: UserId) -> Result<String, ForgeError<T::Error>> {
        let operation = Operation::ReadUser;
        let path = api_path(operation, format!("user/{}", user.0))?;
        let answered: LoginDto = self.api.get(operation, &path)?;
        if answered.id != user.0.get() || answered.login.is_empty() {
            return Err(malformed(operation, "the user answered is another"));
        }
        Ok(answered.login)
    }
}

/// `text`, refused unsent when empty.
fn nonempty<E>(operation: Operation, text: &str) -> Result<&str, ForgeError<E>> {
    if text.is_empty() {
        return Err(ForgeError::InvalidRequest {
            operation,
            detail: "an empty name",
        });
    }
    Ok(text)
}

/// A `Malformed` refusal of `operation`'s response.
const fn malformed<E>(operation: Operation, detail: &'static str) -> ForgeError<E> {
    ForgeError::Malformed { operation, detail }
}

/// A ruleset from its DTO.
fn ruleset<E>(operation: Operation, dto: RulesetDto) -> Result<Ruleset, ForgeError<E>> {
    let target = match dto.target.as_str() {
        "branch" => RulesetTarget::Branch,
        "tag" => RulesetTarget::Tag,
        "push" => RulesetTarget::Push,
        "" => return Err(malformed(operation, "a ruleset has no target")),
        _ => RulesetTarget::Other(dto.target),
    };
    let enforcement = match dto.enforcement.as_str() {
        "active" => Enforcement::Active,
        "evaluate" => Enforcement::Evaluate,
        "disabled" => Enforcement::Disabled,
        _ => return Err(malformed(operation, "unknown ruleset enforcement")),
    };
    let conditions = dto
        .conditions
        .map_or_else(Conditions::default, |conditions| {
            // No `ref_name` condition names no ref.
            let (included, excluded) = conditions
                .ref_name
                .map(|refs| (refs.included, refs.excluded))
                .unwrap_or_default();
            Conditions {
                included,
                excluded,
                other: !conditions.other.is_empty(),
            }
        });
    Ok(Ruleset {
        id: RulesetId(wire::nonzero(operation, dto.id, "a ruleset id is zero")?),
        name: dto.name,
        target,
        enforcement,
        conditions,
        bypass: dto
            .bypass_actors
            .into_iter()
            .map(|actor| bypass_actor(operation, actor))
            .collect::<Result<_, _>>()?,
        rules: dto
            .rules
            .into_iter()
            .map(|rule_dto| rule(operation, rule_dto))
            .collect::<Result<_, _>>()?,
    })
}

/// A bypass actor from its DTO; an actor type GitHub adds later is kept by name.
fn bypass_actor<E>(
    operation: Operation,
    actor: BypassActorDto,
) -> Result<BypassActor, ForgeError<E>> {
    let id = |what| {
        actor
            .actor_id
            .and_then(NonZeroU64::new)
            .ok_or_else(|| malformed(operation, what))
    };
    Ok(match actor.actor_type.as_str() {
        "Integration" => BypassActor::Integration(AppId(id("an app bypass actor has no id")?)),
        "Team" => BypassActor::Team(id("a team bypass actor has no id")?),
        "RepositoryRole" => BypassActor::RepositoryRole(id("a role bypass actor has no id")?),
        "OrganizationAdmin" => BypassActor::OrganizationAdmin,
        "DeployKey" => BypassActor::DeployKey,
        "" => return Err(malformed(operation, "a bypass actor has no type")),
        _ => BypassActor::Other(actor.actor_type),
    })
}

/// A rule from its DTO: the rules doctor reads with their parameters, any other by type.
fn rule<E>(operation: Operation, rule: RuleDto) -> Result<Rule, ForgeError<E>> {
    Ok(match rule.kind.as_str() {
        "required_status_checks" => {
            let parameters: RequiredChecksDto = parameters(operation, rule.parameters)?;
            Rule::RequiredStatusChecks(RequiredChecks {
                checks: parameters
                    .required_status_checks
                    .into_iter()
                    .map(|check| RequiredCheck {
                        context: check.context,
                        // An id of 0 names no app, so the check may come from any source.
                        integration: check.integration_id.and_then(NonZeroU64::new).map(AppId),
                    })
                    .collect(),
                strict: parameters.strict_required_status_checks_policy,
            })
        }
        "file_path_restriction" => {
            let parameters: PathRestrictionDto = parameters(operation, rule.parameters)?;
            Rule::FilePathRestriction(parameters.restricted_file_paths)
        }
        "creation" => Rule::Creation,
        "update" => Rule::Update,
        "deletion" => Rule::Deletion,
        "" => return Err(malformed(operation, "a rule has no type")),
        _ => Rule::Other(rule.kind),
    })
}

/// A rule's `parameters`, decoded as `D`.
fn parameters<D: DeserializeOwned, E>(
    operation: Operation,
    parameters: Option<Value>,
) -> Result<D, ForgeError<E>> {
    let parameters = parameters.ok_or_else(|| malformed(operation, "a rule has no parameters"))?;
    serde_json::from_value(parameters).map_err(|source| ForgeError::Json { operation, source })
}

/// An access level from a role name; an unknown name is a custom role.
fn role<E>(operation: Operation, name: String) -> Result<Access, ForgeError<E>> {
    Ok(match name.as_str() {
        "admin" => Access::Admin,
        "maintain" => Access::Maintain,
        "write" => Access::Write,
        "triage" => Access::Triage,
        "read" => Access::Read,
        "" => return Err(malformed(operation, "a role has no name")),
        _ => Access::Custom(name),
    })
}
