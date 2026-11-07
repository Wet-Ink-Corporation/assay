//! Wire DTOs of the settings doctor reads (OUT-01). Unknown fields are ignored; a missing
//! required field, a wrong type or a repeated key is refused by the decode.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::api::Page;

/// `GET /repos/{owner}/{repo}`.
#[derive(Debug, Deserialize)]
pub(super) struct RepositoryDto {
    pub(super) default_branch: String,
}

/// `GET /repos/{owner}/{repo}/contents/{path}` for a file.
#[derive(Debug, Deserialize)]
pub(super) struct FileDto {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) size: u64,
    pub(super) encoding: Option<String>,
    pub(super) content: Option<String>,
}

/// One entry of `GET /repos/{owner}/{repo}/contents/{path}` for a directory.
#[derive(Debug, Deserialize)]
pub(super) struct EntryDto {
    pub(super) name: String,
    #[serde(rename = "type")]
    pub(super) kind: String,
}

/// One item of `GET /repos/{owner}/{repo}/rulesets`.
#[derive(Debug, Deserialize)]
pub(super) struct RulesetSummaryDto {
    pub(super) id: u64,
}

/// `GET /repos/{owner}/{repo}/rulesets/{id}`. `bypass_actors` is required: GitHub omits it
/// for a token that may not see it, and then nothing can be shown about bypass.
#[derive(Debug, Deserialize)]
pub(super) struct RulesetDto {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) target: String,
    pub(super) enforcement: String,
    pub(super) conditions: Option<ConditionsDto>,
    pub(super) bypass_actors: Vec<BypassActorDto>,
    #[serde(default)]
    pub(super) rules: Vec<RuleDto>,
}

/// A ruleset's `conditions`.
#[derive(Debug, Deserialize)]
pub(super) struct ConditionsDto {
    pub(super) ref_name: Option<RefNameDto>,
    #[serde(flatten)]
    pub(super) other: Map<String, Value>,
}

/// `conditions.ref_name`.
#[derive(Debug, Deserialize)]
pub(super) struct RefNameDto {
    #[serde(default, rename = "include")]
    pub(super) included: Vec<String>,
    #[serde(default, rename = "exclude")]
    pub(super) excluded: Vec<String>,
}

/// One bypass actor.
#[derive(Debug, Deserialize)]
pub(super) struct BypassActorDto {
    pub(super) actor_id: Option<u64>,
    pub(super) actor_type: String,
}

/// One rule, of a ruleset or in force on a branch.
#[derive(Debug, Deserialize)]
pub(super) struct RuleDto {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) parameters: Option<Value>,
}

/// One item of `GET /repos/{owner}/{repo}/rules/branches/{branch}`.
#[derive(Debug, Deserialize)]
pub(super) struct BranchRuleDto {
    #[serde(flatten)]
    pub(super) rule: RuleDto,
    pub(super) ruleset_id: u64,
}

/// The parameters of `required_status_checks`.
#[derive(Debug, Deserialize)]
pub(super) struct RequiredChecksDto {
    pub(super) required_status_checks: Vec<RequiredCheckDto>,
    pub(super) strict_required_status_checks_policy: bool,
}

/// One required check.
#[derive(Debug, Deserialize)]
pub(super) struct RequiredCheckDto {
    pub(super) context: String,
    pub(super) integration_id: Option<u64>,
}

/// The parameters of `file_path_restriction`.
#[derive(Debug, Deserialize)]
pub(super) struct PathRestrictionDto {
    pub(super) restricted_file_paths: Vec<String>,
}

/// `GET /repos/{owner}/{repo}/environments/{name}`.
#[derive(Debug, Deserialize)]
pub(super) struct EnvironmentDto {
    pub(super) deployment_branch_policy: Option<PolicyDto>,
}

/// An environment's `deployment_branch_policy`.
#[derive(Debug, Deserialize)]
pub(super) struct PolicyDto {
    pub(super) protected_branches: bool,
    pub(super) custom_branch_policies: bool,
}

/// A page of `GET .../environments/{name}/deployment-branch-policies`.
#[derive(Debug, Deserialize)]
pub(super) struct BranchPoliciesPage {
    pub(super) branch_policies: Vec<BranchPolicyDto>,
}

impl Page for BranchPoliciesPage {
    type Item = BranchPolicyDto;

    fn items(self) -> Vec<BranchPolicyDto> {
        self.branch_policies
    }
}

/// One deployment branch policy.
#[derive(Debug, Deserialize)]
pub(super) struct BranchPolicyDto {
    pub(super) name: String,
    #[serde(rename = "type")]
    pub(super) kind: String,
}

/// `GET /repos/{owner}/{repo}/actions/permissions/workflow`.
#[derive(Debug, Deserialize)]
pub(super) struct WorkflowPermissionsDto {
    pub(super) can_approve_pull_request_reviews: bool,
}

/// One item of `GET /repos/{owner}/{repo}/collaborators`.
#[derive(Debug, Deserialize)]
pub(super) struct CollaboratorDto {
    pub(super) id: u64,
    pub(super) role_name: String,
}

/// `GET /user/{id}`.
#[derive(Debug, Deserialize)]
pub(super) struct LoginDto {
    pub(super) id: u64,
    pub(super) login: String,
}

/// `GET /repos/{owner}/{repo}/collaborators/{login}/permission`.
#[derive(Debug, Deserialize)]
pub(super) struct PermissionDto {
    pub(super) permission: String,
    pub(super) role_name: String,
    pub(super) user: Option<LoginDto>,
}
