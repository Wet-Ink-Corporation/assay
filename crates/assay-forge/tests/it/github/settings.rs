//! doctor's reads (05 §7, #75) from recorded responses: the repository, its rulesets (recorded
//! from this repository, in evaluate mode), the rules in force on a branch, files and
//! directories, environments, the Actions token's rights, collaborators and a user's access.
//! Bodies the proxy would not let this session read follow GitHub's documented schema.

use assay_app::{
    Access, AppId, AppliedRule, BranchPolicy, BypassActor, Collaborator, Conditions,
    DeploymentBranches, DirectoryEntry, Enforcement, EntryType, PolicyKind, PrerequisiteForge,
    RequiredCheck, RequiredChecks, Rule, RulesetTarget, UserId,
};
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::json;

use super::recorded::{
    REPOSITORY, Recorded, Waits, fixture, fixture_json, github, id, json, path, reply,
};

fn user(value: u64) -> UserId {
    UserId(id(value))
}

#[test]
fn the_default_branch_is_read_from_the_repository() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &format!("repos/{REPOSITORY}"),
        fixture("repository.json"),
    );
    let waits = Waits::default();

    assert_eq!(
        github(&recorded, &waits).default_branch().expect("read"),
        "main"
    );
}

#[test]
fn every_ruleset_is_listed_then_read_in_full() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("rulesets?includes_parents=true&per_page=100"),
            fixture("rulesets.json"),
        )
        .ok(
            Method::Get,
            &path("rulesets/25356356"),
            fixture("ruleset_25356356.json"),
        )
        .ok(
            Method::Get,
            &path("rulesets/25356300"),
            fixture("ruleset_25356300.json"),
        );
    let waits = Waits::default();

    let rulesets = github(&recorded, &waits).rulesets().expect("read");

    assert_eq!(rulesets.len(), 2);
    let push = &rulesets[0];
    assert_eq!(push.target, RulesetTarget::Push);
    assert_eq!(push.enforcement, Enforcement::Evaluate);
    assert_eq!(push.conditions, Conditions::default());
    assert!(push.bypass.is_empty());
    assert_eq!(
        push.rules,
        vec![Rule::FilePathRestriction(
            [
                ".github/workflows/**",
                ".assay/**",
                ".claude/**",
                "CODEOWNERS",
                ".gitattributes"
            ]
            .map(str::to_owned)
            .to_vec()
        )]
    );
    let main = &rulesets[1];
    assert_eq!(main.id.0.get(), 25_356_300);
    assert_eq!(main.target, RulesetTarget::Branch);
    assert_eq!(main.conditions.included, vec!["~DEFAULT_BRANCH".to_owned()]);
    assert_eq!(
        main.rules,
        vec![
            Rule::Deletion,
            Rule::Other("non_fast_forward".to_owned()),
            Rule::RequiredStatusChecks(RequiredChecks {
                checks: vec![RequiredCheck {
                    context: "assay / verdict".to_owned(),
                    integration: Some(AppId(id(5_371_952))),
                }],
                strict: true,
            }),
        ]
    );
    assert_eq!(recorded.unused(), 0);
}

#[test]
fn a_ruleset_without_its_bypass_list_is_refused() {
    let mut ruleset = fixture_json("ruleset_25356300.json");
    ruleset
        .as_object_mut()
        .expect("an object")
        .remove("bypass_actors");
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("rulesets?includes_parents=true&per_page=100"),
            json(&json!([{ "id": 25_356_300 }])),
        )
        .ok(Method::Get, &path("rulesets/25356300"), json(&ruleset));
    let waits = Waits::default();

    let read = github(&recorded, &waits).rulesets();

    assert!(
        matches!(
            read,
            Err(ForgeError::Json {
                operation: Operation::ReadRuleset,
                ..
            })
        ),
        "{read:?}"
    );
}

#[test]
fn bypass_actors_and_other_conditions_are_read_by_type() {
    let mut ruleset = fixture_json("ruleset_25356300.json");
    ruleset["bypass_actors"] = json!([
        { "actor_id": 5_400_000, "actor_type": "Integration", "bypass_mode": "always" },
        { "actor_id": 7, "actor_type": "Team", "bypass_mode": "pull_request" },
        { "actor_id": 1, "actor_type": "OrganizationAdmin", "bypass_mode": "always" },
        { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" },
        { "actor_id": null, "actor_type": "DeployKey", "bypass_mode": "always" },
        { "actor_id": 3, "actor_type": "EnterpriseOwner", "bypass_mode": "always" }
    ]);
    ruleset["conditions"]["repository_name"] = json!({ "include": ["~ALL"], "exclude": [] });
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("rulesets?includes_parents=true&per_page=100"),
            json(&json!([{ "id": 25_356_300 }])),
        )
        .ok(Method::Get, &path("rulesets/25356300"), json(&ruleset));
    let waits = Waits::default();

    let rulesets = github(&recorded, &waits).rulesets().expect("read");

    assert_eq!(
        rulesets[0].bypass,
        vec![
            BypassActor::Integration(AppId(id(5_400_000))),
            BypassActor::Team(id(7)),
            BypassActor::OrganizationAdmin,
            BypassActor::RepositoryRole(id(5)),
            BypassActor::DeployKey,
            BypassActor::Other("EnterpriseOwner".to_owned()),
        ]
    );
    assert!(rulesets[0].conditions.other);
}

#[test]
fn an_unknown_enforcement_is_refused() {
    let mut ruleset = fixture_json("ruleset_25356300.json");
    ruleset["enforcement"] = json!("sometimes");
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("rulesets?includes_parents=true&per_page=100"),
            json(&json!([{ "id": 25_356_300 }])),
        )
        .ok(Method::Get, &path("rulesets/25356300"), json(&ruleset));
    let waits = Waits::default();

    let read = github(&recorded, &waits).rulesets();

    assert!(
        matches!(
            read,
            Err(ForgeError::Malformed {
                operation: Operation::ReadRuleset,
                ..
            })
        ),
        "{read:?}"
    );
}

#[test]
fn the_rules_in_force_on_a_branch_carry_their_ruleset() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("rules/branches/release%2F1?per_page=100"),
        fixture("rules_branch_main.json"),
    );
    let waits = Waits::default();

    let rules = github(&recorded, &waits)
        .branch_rules("release/1")
        .expect("read");

    let ruleset = assay_app::RulesetId(id(25_356_300));
    assert_eq!(
        rules,
        vec![
            AppliedRule {
                ruleset,
                rule: Rule::Deletion
            },
            AppliedRule {
                ruleset,
                rule: Rule::RequiredStatusChecks(RequiredChecks {
                    checks: vec![
                        RequiredCheck {
                            context: "assay / verdict".to_owned(),
                            integration: Some(AppId(id(5_371_952))),
                        },
                        RequiredCheck {
                            context: "gate".to_owned(),
                            integration: None
                        },
                    ],
                    strict: true,
                }),
            },
        ]
    );
}

#[test]
fn a_file_is_decoded_from_base64() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("contents/.github/workflows/warm-target.yml?ref=main"),
        fixture("contents_warm_target.json"),
    );
    let waits = Waits::default();

    let bytes = github(&recorded, &waits)
        .file("main", ".github/workflows/warm-target.yml")
        .expect("read")
        .expect("the file exists");

    assert_eq!(bytes.len(), 8811);
    assert!(
        bytes.starts_with(b"# Warm target/"),
        "{:?}",
        String::from_utf8_lossy(&bytes[..40])
    );
}

#[test]
fn a_file_that_is_absent_is_none() {
    let recorded = Recorded::default().then(
        Method::Get,
        &path("contents/.assay/owners?ref=main"),
        reply(404, &[], json(&json!({ "message": "Not Found" }))),
    );
    let waits = Waits::default();

    assert_eq!(
        github(&recorded, &waits)
            .file("main", ".assay/owners")
            .expect("read"),
        None
    );
}

#[test]
fn a_file_whose_content_does_not_match_its_size_or_encoding_is_refused() {
    let mut wrong_size = fixture_json("contents_warm_target.json");
    wrong_size["size"] = json!(8810);
    let mut not_base64 = fixture_json("contents_warm_target.json");
    not_base64["content"] = json!("IyBX*Fy");
    let mut too_large = fixture_json("contents_warm_target.json");
    too_large["encoding"] = json!("none");
    too_large["content"] = json!("");
    for body in [wrong_size, not_base64, too_large] {
        let recorded = Recorded::default().ok(
            Method::Get,
            &path("contents/.github/workflows/warm-target.yml?ref=main"),
            json(&body),
        );
        let waits = Waits::default();

        let read = github(&recorded, &waits).file("main", ".github/workflows/warm-target.yml");

        assert!(
            matches!(
                read,
                Err(ForgeError::Malformed {
                    operation: Operation::ReadContents,
                    ..
                })
            ),
            "{read:?}"
        );
    }
}

#[test]
fn a_directory_lists_its_entries() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("contents/.github/workflows?ref=main"),
        fixture("contents_workflows.json"),
    );
    let waits = Waits::default();

    let entries = github(&recorded, &waits)
        .directory("main", ".github/workflows")
        .expect("read")
        .expect("the directory exists");

    let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["release.yml", "rust.yml", "warm-target.yml"]);
    assert!(entries.iter().all(|entry| entry.kind == EntryType::File));
    let _: &[DirectoryEntry] = &entries;
}

#[test]
fn a_path_that_could_leave_its_directory_is_refused_unsent() {
    let recorded = Recorded::default();
    let waits = Waits::default();

    for bad in ["../x", ".github//workflows", "", "/abs", ".github/./w"] {
        let read = github(&recorded, &waits).file("main", bad);

        assert!(
            matches!(
                read,
                Err(ForgeError::InvalidRequest {
                    operation: Operation::ReadContents,
                    ..
                })
            ),
            "{bad:?}: {read:?}"
        );
    }
    assert!(recorded.sent().is_empty());
}

#[test]
fn environments_are_read_with_their_branch_policies() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("environments/assay-publish"),
            fixture("environment_custom.json"),
        )
        .ok(
            Method::Get,
            &path("environments/assay-publish/deployment-branch-policies?per_page=100"),
            fixture("branch_policies.json"),
        )
        .ok(
            Method::Get,
            &path("environments/assay-review"),
            fixture("environment_all.json"),
        )
        .ok(
            Method::Get,
            &path("environments/assay-review"),
            fixture("environment_protected.json"),
        )
        .then(
            Method::Get,
            &path("environments/assay-gone"),
            reply(404, &[], json(&json!({ "message": "Not Found" }))),
        );
    let waits = Waits::default();
    let forge = github(&recorded, &waits);

    assert_eq!(
        forge.environment("assay-publish").expect("read"),
        Some(DeploymentBranches::Custom(vec![BranchPolicy {
            name: "main".to_owned(),
            kind: PolicyKind::Branch,
        }]))
    );
    assert_eq!(
        forge.environment("assay-review").expect("read"),
        Some(DeploymentBranches::All)
    );
    assert_eq!(
        forge.environment("assay-review").expect("read"),
        Some(DeploymentBranches::Protected)
    );
    assert_eq!(forge.environment("assay-gone").expect("read"), None);
}

#[test]
fn a_branch_policy_without_a_known_type_is_refused() {
    let mut policies = fixture_json("branch_policies.json");
    policies["branch_policies"][0]["type"] = json!("ref");
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            &path("environments/assay-publish"),
            fixture("environment_custom.json"),
        )
        .ok(
            Method::Get,
            &path("environments/assay-publish/deployment-branch-policies?per_page=100"),
            json(&policies),
        );
    let waits = Waits::default();

    let read = github(&recorded, &waits).environment("assay-publish");

    assert!(
        matches!(
            read,
            Err(ForgeError::Malformed {
                operation: Operation::ListBranchPolicies,
                ..
            })
        ),
        "{read:?}"
    );
}

#[test]
fn the_actions_tokens_rights_are_read() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("actions/permissions/workflow"),
        fixture("workflow_permissions.json"),
    );
    let waits = Waits::default();

    let permissions = github(&recorded, &waits)
        .workflow_permissions()
        .expect("read");

    assert!(!permissions.can_approve_pull_requests);
}

#[test]
fn collaborators_are_read_with_their_roles() {
    let recorded = Recorded::default().ok(
        Method::Get,
        &path("collaborators?affiliation=all&per_page=100"),
        fixture("collaborators.json"),
    );
    let waits = Waits::default();

    let collaborators = github(&recorded, &waits).collaborators().expect("read");

    assert_eq!(
        collaborators,
        vec![
            Collaborator {
                user: user(124_330_161),
                access: Access::Admin
            },
            Collaborator {
                user: user(1001),
                access: Access::Custom("release-manager".to_owned()),
            },
        ]
    );
}

#[test]
fn a_users_access_is_read_by_their_login_and_checked_by_id() {
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            "user/124330161",
            fixture("user_124330161.json"),
        )
        .ok(
            Method::Get,
            &path("collaborators/Ryan-Britton/permission"),
            fixture("permission_admin.json"),
        )
        .ok(
            Method::Get,
            "user/124330161",
            fixture("user_124330161.json"),
        )
        .ok(
            Method::Get,
            &path("collaborators/Ryan-Britton/permission"),
            fixture("permission_none.json"),
        );
    let waits = Waits::default();
    let forge = github(&recorded, &waits);

    assert_eq!(
        forge.access(user(124_330_161)).expect("read"),
        Access::Admin
    );
    assert_eq!(forge.access(user(124_330_161)).expect("read"), Access::None);
}

#[test]
fn a_permission_answered_for_another_user_is_refused() {
    let mut permission = fixture_json("permission_admin.json");
    permission["user"]["id"] = json!(1001);
    let recorded = Recorded::default()
        .ok(
            Method::Get,
            "user/124330161",
            fixture("user_124330161.json"),
        )
        .ok(
            Method::Get,
            &path("collaborators/Ryan-Britton/permission"),
            json(&permission),
        );
    let waits = Waits::default();

    let read = github(&recorded, &waits).access(user(124_330_161));

    assert!(
        matches!(
            read,
            Err(ForgeError::Malformed {
                operation: Operation::ReadPermission,
                ..
            })
        ),
        "{read:?}"
    );
}
