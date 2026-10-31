//! doctor's repository checks (05 §7, #75) over a fake repository and ledger: a healthy scene
//! passes every prerequisite, and each failing fixture fails exactly the prerequisite it breaks.

use assay_app::{
    Access, BaseError, BranchPolicy, BypassActor, Collaborator, DeploymentBranches, DirectoryEntry,
    Enforcement, EntryType, Holding, IdentityLists, ListFile, ListName, ListsError,
    MAX_LIST_ENTRIES, PolicyKind, Prerequisite, PrerequisiteFailure, PrerequisiteReport, Rule,
    RulesetTarget, WorkflowError, prerequisites, read_base,
};

/// The lists `.assay/owners` must hold besides `owners`.
const OTHER_OWNERS_LISTS: &str = "approvers = []\nmaintainers = []\n";

use crate::plan_fakes::FakeError;
use crate::prerequisites_fakes::{
    APPROVER, DAILY, FakeRepository, MAIN, OWNER, RELEASE, SHA, apps, assay_yml, plugins, user,
    verdict_rule,
};

type Failure = PrerequisiteFailure<FakeError>;

/// The report for `repository` and `ledger`, with base config read from `repository`.
fn report(
    repository: &FakeRepository,
    ledger: Option<&FakeRepository>,
) -> PrerequisiteReport<FakeError> {
    let base = read_base(repository).expect("base config is readable");
    prerequisites(repository, ledger, &base, apps())
}

/// Each prerequisite that failed, in report order.
fn failed(report: &PrerequisiteReport<FakeError>) -> Vec<Prerequisite> {
    report
        .results()
        .iter()
        .filter(|result| result.failure.is_some())
        .map(|result| result.prerequisite)
        .collect()
}

/// Asserts that `prerequisite` is the only one failing, and that its failure is `expected`.
fn assert_fails(
    repository: &FakeRepository,
    ledger: Option<&FakeRepository>,
    prerequisite: Prerequisite,
    expected: impl Fn(&Failure) -> bool,
) {
    let report = report(repository, ledger);
    assert_eq!(failed(&report), vec![prerequisite], "{report:#?}");
    assert!(!report.passed(), "a failure fails the report");
    let failure = report
        .results()
        .iter()
        .find_map(|result| result.failure.as_ref())
        .expect("one prerequisite failed");
    assert!(expected(failure), "{prerequisite}: {failure:?}");
}

fn healthy() -> (FakeRepository, FakeRepository) {
    (FakeRepository::healthy(), FakeRepository::healthy_ledger())
}

#[test]
fn a_repository_meeting_every_prerequisite_passes_each_in_order() {
    let (repository, ledger) = healthy();

    let report = report(&repository, Some(&ledger));

    let reported: Vec<_> = report
        .results()
        .iter()
        .map(|result| result.prerequisite)
        .collect();
    assert_eq!(reported, Prerequisite::ALL.to_vec());
    assert!(report.passed(), "{report:#?}");
}

#[test]
fn a_base_without_its_required_verdict_check_fails() {
    let (mut repository, ledger) = healthy();
    repository.branch_rules.remove(RELEASE);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::VerdictRuleset,
        |failure| {
            matches!(failure, Failure::CheckNotRequired { base, check }
            if base == RELEASE && check.as_str() == "assay / verdict @release")
        },
    );
}

#[test]
fn the_default_branchs_check_name_on_another_base_does_not_count() {
    let (mut repository, ledger) = healthy();
    repository.branch_rules.insert(
        RELEASE.to_owned(),
        vec![verdict_rule(12, "assay / verdict", true)],
    );

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::VerdictRuleset,
        |failure| matches!(failure, Failure::CheckNotRequired { base, .. } if base == RELEASE),
    );
}

#[test]
fn a_verdict_check_from_another_app_does_not_count() {
    let (mut repository, ledger) = healthy();
    let mut rule = verdict_rule(10, "assay / verdict", true);
    if let Rule::RequiredStatusChecks(required) = &mut rule.rule {
        required.checks[0].integration = None;
    }
    repository.branch_rules.insert(MAIN.to_owned(), vec![rule]);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::VerdictRuleset,
        |failure| matches!(failure, Failure::CheckNotRequired { base, .. } if base == MAIN),
    );
}

#[test]
fn a_verdict_rule_without_branches_up_to_date_fails() {
    let (mut repository, ledger) = healthy();
    repository.branch_rules.insert(
        MAIN.to_owned(),
        vec![verdict_rule(10, "assay / verdict", false)],
    );

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::VerdictRuleset,
        |failure| matches!(failure, Failure::NotUpToDate { base, ruleset } if base == MAIN && ruleset.0.get() == 10),
    );
}

#[test]
fn a_verdict_ruleset_with_a_bypass_actor_fails() {
    let (mut repository, ledger) = healthy();
    repository.rulesets[0].bypass = vec![BypassActor::OrganizationAdmin];

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::VerdictRuleset,
        |failure| {
            matches!(
                failure,
                Failure::Bypass { ruleset, actor: BypassActor::OrganizationAdmin } if ruleset.0.get() == 10
            )
        },
    );
}

#[test]
fn a_verdict_ruleset_that_is_not_active_fails() {
    let (mut repository, ledger) = healthy();
    repository.rulesets[0].enforcement = Enforcement::Evaluate;

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::VerdictRuleset,
        |failure| matches!(failure, Failure::NotActive { ruleset } if ruleset.0.get() == 10),
    );
}

#[test]
fn a_rule_from_a_ruleset_the_list_does_not_hold_fails() {
    let (mut repository, ledger) = healthy();
    repository.branch_rules.insert(
        MAIN.to_owned(),
        vec![verdict_rule(99, "assay / verdict", true)],
    );

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::VerdictRuleset,
        |failure| matches!(failure, Failure::UnknownRuleset { ruleset } if ruleset.0.get() == 99),
    );
}

#[test]
fn a_default_branch_outside_protected_bases_fails() {
    let (mut repository, ledger) = healthy();
    let config = plugins(crate::prerequisites_fakes::DIGEST)
        .replace("[\"main\", \"release\"]", "[\"release\"]");
    repository.put(MAIN, ".assay/plugins.toml", &config);

    let report = report(&repository, Some(&ledger));

    let failure = report.results()[0].failure.as_ref();
    assert!(
        matches!(failure, Some(Failure::DefaultNotProtected { branch }) if branch == MAIN),
        "{failure:?}"
    );
}

#[test]
fn a_protected_base_without_config_fails_the_digest_check() {
    let (mut repository, ledger) = healthy();
    repository
        .files
        .remove(&(RELEASE.to_owned(), ".assay/plugins.toml".to_owned()));

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::BaseDigest,
        |failure| matches!(failure, Failure::BaseConfigMissing { base } if base == RELEASE),
    );
}

#[test]
fn a_protected_base_pinning_another_digest_fails() {
    let (mut repository, ledger) = healthy();
    let other = format!("sha256:{}", "0".repeat(64));
    repository.put(RELEASE, ".assay/plugins.toml", &plugins(&other));

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::BaseDigest,
        |failure| matches!(failure, Failure::DigestDiffers { base } if base == RELEASE),
    );
}

#[test]
fn a_protected_base_with_a_refused_config_fails() {
    let (mut repository, ledger) = healthy();
    repository.put(RELEASE, ".assay/plugins.toml", "[assay]\n");

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::BaseDigest,
        |failure| matches!(failure, Failure::BaseConfig { base, .. } if base == RELEASE),
    );
}

#[test]
fn an_installed_workflow_triggered_from_the_merge_commit_fails() {
    let (mut repository, ledger) = healthy();
    let review = "on: pull_request_review\njobs:\n  decide:\n    if: github.run_attempt == 1\n    runs-on: x\n";
    repository.workflow("assay-feedback.yml", review);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::MergeCommitTrigger,
        |failure| {
            matches!(failure, Failure::MergeCommitTrigger { path, event }
            if path == ".github/workflows/assay-feedback.yml" && event == "pull_request_review")
        },
    );
}

#[test]
fn an_installed_workflow_triggered_by_an_event_outside_the_allowlist_fails() {
    for event in [
        "workflow_call",
        "merge_group",
        "pull_request_targets",
        "Push",
    ] {
        let (mut repository, ledger) = healthy();
        let text = format!(
            "on: [schedule, {event}]\njobs:\n  a:\n    if: github.run_attempt == 1\n    runs-on: x\n"
        );
        repository.workflow("assay-extra.yml", &text);

        assert_fails(
            &repository,
            Some(&ledger),
            Prerequisite::MergeCommitTrigger,
            |failure| {
                matches!(failure, Failure::MergeCommitTrigger { path, event: found }
                if path == ".github/workflows/assay-extra.yml" && found == event)
            },
        );
    }
}

#[test]
fn every_allowlisted_event_passes() {
    let (mut repository, ledger) = healthy();
    let text = "on: [pull_request_target, issue_comment, schedule, workflow_dispatch]\njobs:\n  a:\n    if: github.run_attempt == 1\n    runs-on: x\n";
    repository.workflow("assay-extra.yml", text);

    let report = report(&repository, Some(&ledger));

    assert!(report.passed(), "{report:#?}");
}

#[test]
fn no_installed_workflow_fails_the_installed_workflow_checks() {
    let (mut repository, ledger) = healthy();
    repository.directories.clear();

    let report = report(&repository, Some(&ledger));

    assert_eq!(
        failed(&report),
        vec![
            Prerequisite::MergeCommitTrigger,
            Prerequisite::RunAttemptGuard
        ]
    );
    let workflow_failures: Vec<_> = report
        .results()
        .iter()
        .filter_map(|result| result.failure.as_ref())
        .collect();
    assert!(
        workflow_failures
            .iter()
            .all(|failure| matches!(failure, Failure::NoWorkflows)),
        "{workflow_failures:?}"
    );
}

#[test]
fn an_unreadable_installed_workflow_fails_every_check_that_reads_it() {
    let (mut repository, ledger) = healthy();
    repository.put(
        MAIN,
        ".github/workflows/assay.yml",
        "on: &x push\njobs: {}\n",
    );

    let report = report(&repository, Some(&ledger));

    assert_eq!(
        failed(&report),
        vec![
            Prerequisite::MergeCommitTrigger,
            Prerequisite::PinnedActions,
            Prerequisite::RunAttemptGuard
        ]
    );
    let failure = report.results()[2].failure.as_ref();
    assert!(
        matches!(failure, Some(Failure::Workflow { path, source: WorkflowError::Unreadable(_) })
            if path == ".github/workflows/assay.yml"),
        "{failure:?}"
    );
}

#[test]
fn a_workflow_entry_that_is_not_a_file_fails() {
    let (mut repository, ledger) = healthy();
    repository.directories.values_mut().for_each(|entries| {
        entries.push(DirectoryEntry {
            name: "assay-link.yml".to_owned(),
            kind: EntryType::Symlink,
        });
    });

    let report = report(&repository, Some(&ledger));

    let failure = report.results()[2].failure.as_ref();
    assert!(
        matches!(failure, Some(Failure::WorkflowNotAFile { path }) if path.ends_with("assay-link.yml")),
        "{failure:?}"
    );
}

#[test]
fn a_trust_path_without_a_push_restriction_fails() {
    let (mut repository, ledger) = healthy();
    repository.rulesets[1].rules = vec![Rule::FilePathRestriction(vec![
        ".github/workflows/**".to_owned(),
        ".assay/**".to_owned(),
        "CODEOWNERS".to_owned(),
        ".gitattributes".to_owned(),
    ])];

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::PathRestriction,
        |failure| matches!(failure, Failure::PathUnrestricted { path: ".claude/**" }),
    );
}

#[test]
fn a_push_ruleset_only_evaluated_restricts_nothing() {
    let (mut repository, ledger) = healthy();
    repository.rulesets[1].enforcement = Enforcement::Evaluate;

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::PathRestriction,
        |failure| {
            matches!(
                failure,
                Failure::PathUnrestricted {
                    path: ".github/workflows/**"
                }
            )
        },
    );
}

#[test]
fn an_agent_in_owners_fails_the_identity_lists() {
    let (mut repository, ledger) = healthy();
    let owners = format!(
        "owners = [{{ id = {DAILY}, login = \"Ryan-Britton\" }}]\napprovers = [{{ id = {APPROVER}, login = \"a\" }}]\nmaintainers = []\n"
    );
    repository.put(MAIN, ".assay/owners", &owners);
    // The ledger's only collaborator is no longer an owner either.
    let ledger = FakeRepository {
        collaborators: Vec::new(),
        ..ledger
    };

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::IdentityLists,
        |failure| matches!(failure, Failure::AgentPrivileged { user: agent } if *agent == user(DAILY)),
    );
}

#[test]
fn a_maintainer_who_is_an_approver_fails_the_identity_lists() {
    let (mut repository, ledger) = healthy();
    let owners = format!(
        "owners = [{{ id = {OWNER}, login = \"o\" }}]\napprovers = [{{ id = {APPROVER}, login = \"a\" }}]\nmaintainers = [{{ id = {APPROVER}, login = \"a\" }}]\n"
    );
    repository.put(MAIN, ".assay/owners", &owners);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::IdentityLists,
        |failure| matches!(failure, Failure::MaintainerPrivileged { user: maintainer } if *maintainer == user(APPROVER)),
    );
}

#[test]
fn no_approver_fails_loudly_as_on_this_repository_today() {
    let (mut repository, ledger) = healthy();
    // docs/decisions/0005: the approver is deferred to #81, so `approvers` is empty.
    let owners = format!(
        "owners = [{{ id = {OWNER}, login = \"o\" }}]\napprovers = []\nmaintainers = [{{ id = {DAILY}, login = \"Ryan-Britton\" }}]\n"
    );
    repository.put(MAIN, ".assay/owners", &owners);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::Approver,
        |failure| matches!(failure, Failure::NoApprover),
    );
}

#[test]
fn an_approver_who_is_an_agent_fails() {
    let (mut repository, ledger) = healthy();
    let agents = format!(
        "agents = [{{ id = {DAILY}, login = \"d\" }}, {{ id = {APPROVER}, login = \"a\" }}]"
    );
    repository.put(MAIN, ".assay/agents", &agents);
    repository.access.insert(user(APPROVER), Access::Write);
    let mut ledger = ledger;
    ledger.access.insert(user(APPROVER), Access::None);

    let report = report(&repository, Some(&ledger));

    // The approver-agent is also privileged, which the identity lists report.
    assert_eq!(
        failed(&report),
        vec![Prerequisite::IdentityLists, Prerequisite::Approver]
    );
    let failure = report.results()[5].failure.as_ref();
    assert!(
        matches!(failure, Some(Failure::ApproverIsAgent { user: approver }) if *approver == user(APPROVER)),
        "{failure:?}"
    );
}

#[test]
fn an_agent_who_administers_the_repository_fails() {
    let (mut repository, ledger) = healthy();
    // The daily account is an org owner (docs/decisions/0005), so GitHub reports admin.
    repository.access.insert(user(DAILY), Access::Admin);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::AgentAccess,
        |failure| matches!(failure, Failure::AgentAccess { user: agent, access: Access::Admin } if *agent == user(DAILY)),
    );
}

#[test]
fn an_agent_with_a_custom_role_is_not_shown_to_be_harmless() {
    let (mut repository, ledger) = healthy();
    repository
        .access
        .insert(user(DAILY), Access::Custom("release-manager".to_owned()));

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::AgentAccess,
        |failure| {
            matches!(
                failure,
                Failure::AgentAccess {
                    access: Access::Custom(_),
                    ..
                }
            )
        },
    );
}

#[test]
fn actions_that_may_approve_pull_requests_fail() {
    let (mut repository, ledger) = healthy();
    repository.can_approve = true;

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::ActionsApproval,
        |failure| matches!(failure, Failure::ActionsCanApprove),
    );
}

#[test]
fn an_action_pinned_by_tag_fails() {
    let (mut repository, ledger) = healthy();
    let text = "on: push\npermissions: {}\njobs:\n  a:\n    runs-on: x\n    steps:\n      - uses: actions/setup-node@v4\n";
    repository.workflow("ci.yml", text);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::PinnedActions,
        |failure| {
            matches!(failure, Failure::Unpinned { path, uses }
            if path == ".github/workflows/ci.yml" && uses == "actions/setup-node@v4")
        },
    );
}

#[test]
fn a_missing_environment_fails() {
    let (mut repository, ledger) = healthy();
    repository.environments.remove("assay-review");

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::Environments,
        |failure| {
            matches!(
                failure,
                Failure::EnvironmentMissing {
                    name: "assay-review"
                }
            )
        },
    );
}

#[test]
fn an_environment_open_to_more_than_the_default_branch_fails() {
    let open = [
        DeploymentBranches::All,
        DeploymentBranches::Protected,
        DeploymentBranches::Custom(vec![BranchPolicy {
            name: "*".to_owned(),
            kind: PolicyKind::Branch,
        }]),
        DeploymentBranches::Custom(vec![
            BranchPolicy {
                name: MAIN.to_owned(),
                kind: PolicyKind::Branch,
            },
            BranchPolicy {
                name: "v*".to_owned(),
                kind: PolicyKind::Tag,
            },
        ]),
    ];
    for branches in open {
        let (mut repository, ledger) = healthy();
        repository
            .environments
            .insert("assay-publish".to_owned(), branches.clone());

        assert_fails(
            &repository,
            Some(&ledger),
            Prerequisite::Environments,
            |failure| {
                matches!(
                    failure,
                    Failure::DeploymentBranches {
                        name: "assay-publish"
                    }
                )
            },
        );
    }
}

#[test]
fn a_privileged_job_without_the_guard_fails() {
    let (mut repository, ledger) = healthy();
    let unguarded = assay_yml().replace("    if: github.run_attempt == 1\n", "");
    repository.put(MAIN, ".github/workflows/assay.yml", &unguarded);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::RunAttemptGuard,
        |failure| {
            matches!(failure, Failure::Unguarded { path, job, holds: Holding::Environment }
            if path == ".github/workflows/assay.yml" && job == "review")
        },
    );
}

#[test]
fn a_job_with_no_declared_permissions_needs_the_guard() {
    let (mut repository, ledger) = healthy();
    let text = format!(
        "on: issue_comment\njobs:\n  reply:\n    runs-on: x\n    steps:\n      - uses: actions/checkout@{SHA}\n"
    );
    repository.workflow("assay-reply.yml", &text);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::RunAttemptGuard,
        |failure| matches!(failure, Failure::Unguarded { job, holds: Holding::WriteToken, .. } if job == "reply"),
    );
}

#[test]
fn no_ledger_configured_fails_every_ledger_check() {
    let (repository, _) = healthy();

    let report = report(&repository, None);

    assert_eq!(
        failed(&report),
        vec![
            Prerequisite::LedgerCollaborators,
            Prerequisite::LedgerRuleset,
            Prerequisite::LedgerAgents
        ]
    );
    assert!(
        report
            .results()
            .iter()
            .filter_map(|result| result.failure.as_ref())
            .all(|failure| matches!(failure, Failure::NoLedger))
    );
}

#[test]
fn a_ledger_collaborator_who_is_not_an_owner_fails() {
    let (repository, mut ledger) = healthy();
    ledger.collaborators.push(Collaborator {
        user: user(DAILY),
        access: Access::Admin,
    });

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::LedgerCollaborators,
        |failure| matches!(failure, Failure::LedgerCollaborator { user: stranger } if *stranger == user(DAILY)),
    );
}

#[test]
fn a_ledger_ruleset_that_misses_a_restriction_fails() {
    let broken: [fn(&mut assay_app::Ruleset); 6] = [
        |ruleset| ruleset.enforcement = Enforcement::Evaluate,
        |ruleset| ruleset.target = RulesetTarget::Push,
        |ruleset| ruleset.conditions.included = vec!["~DEFAULT_BRANCH".to_owned()],
        |ruleset| ruleset.conditions.excluded = vec!["refs/heads/slot/*".to_owned()],
        |ruleset| ruleset.rules.retain(|rule| *rule != Rule::Update),
        |ruleset| ruleset.bypass.push(BypassActor::OrganizationAdmin),
    ];
    for (index, breaks) in broken.into_iter().enumerate() {
        let (repository, mut ledger) = healthy();
        breaks(&mut ledger.rulesets[0]);

        let report = report(&repository, Some(&ledger));

        assert_eq!(
            failed(&report),
            vec![Prerequisite::LedgerRuleset],
            "case {index}"
        );
        assert!(matches!(
            report.results()[12].failure,
            Some(Failure::LedgerUnrestricted)
        ));
    }
}

#[test]
fn an_agent_with_read_access_to_the_ledger_fails() {
    let (repository, mut ledger) = healthy();
    ledger.access.insert(user(DAILY), Access::Read);

    assert_fails(
        &repository,
        Some(&ledger),
        Prerequisite::LedgerAgents,
        |failure| matches!(failure, Failure::LedgerAgentAccess { user: agent, access: Access::Read } if *agent == user(DAILY)),
    );
}

#[test]
fn a_read_that_fails_fails_only_the_checks_that_need_it() {
    let (mut repository, ledger) = healthy();
    repository.broken.insert("environment");
    repository.broken.insert("workflow_permissions");

    let report = report(&repository, Some(&ledger));

    assert_eq!(
        failed(&report),
        vec![Prerequisite::ActionsApproval, Prerequisite::Environments]
    );
    assert!(
        report
            .results()
            .iter()
            .filter_map(|result| result.failure.as_ref())
            .all(|failure| matches!(failure, Failure::Unreadable(FakeError)))
    );
}

#[test]
fn base_config_that_cannot_be_read_is_an_error_not_a_report() {
    let (mut repository, _) = healthy();
    repository
        .files
        .remove(&(MAIN.to_owned(), ".assay/owners".to_owned()));

    let read = read_base(&repository);

    assert!(
        matches!(&read, Err(BaseError::Missing { path: ".assay/owners", branch }) if branch == MAIN),
        "{read:?}"
    );

    let (mut broken, _) = healthy();
    broken.broken.insert("default_branch");
    assert!(matches!(
        read_base(&broken),
        Err(BaseError::Forge(FakeError))
    ));

    let (mut refused, _) = healthy();
    refused.put(MAIN, ".assay/plugins.toml", "not toml");
    assert!(matches!(read_base(&refused), Err(BaseError::Config(_))));
}

#[test]
fn the_identity_lists_are_read_by_id() {
    let lists = IdentityLists::parse(
        crate::prerequisites_fakes::OWNERS,
        crate::prerequisites_fakes::AGENTS,
    )
    .expect("the lists are valid");

    assert_eq!(
        lists.owners().iter().copied().collect::<Vec<_>>(),
        vec![user(OWNER)]
    );
    assert_eq!(
        lists.approvers().iter().copied().collect::<Vec<_>>(),
        vec![user(APPROVER)]
    );
    assert_eq!(
        lists.maintainers().iter().copied().collect::<Vec<_>>(),
        vec![user(DAILY)]
    );
    assert_eq!(
        lists.agents().iter().copied().collect::<Vec<_>>(),
        vec![user(DAILY)]
    );
}

/// What an identity-list refusal must be.
type Expected = fn(&ListsError) -> bool;

/// `.assay/owners` with `owners` set to `owners` and the other lists empty.
fn owners_file(owners: &str) -> String {
    format!("owners = [{owners}]\n{OTHER_OWNERS_LISTS}")
}

/// Whether `error` refuses `.assay/owners` as TOML.
fn owners_toml(error: &ListsError) -> bool {
    matches!(
        error,
        ListsError::Toml {
            file: ListFile::Owners,
            ..
        }
    )
}

/// Whether `error` refuses user 1's login in `owners`.
fn owner_login(error: &ListsError) -> bool {
    matches!(error, ListsError::Login { list: ListName::Owners, id } if id.0.get() == 1)
}

/// Asserts `IdentityLists::parse` refuses each case as expected.
fn assert_refused(cases: &[(&str, &str, Expected)]) {
    for &(owners, agents, expected) in cases {
        let parsed = IdentityLists::parse(owners, agents);

        assert!(
            matches!(&parsed, Err(error) if expected(error)),
            "{owners} / {agents}: {parsed:?}"
        );
    }
}

/// doctor reads the lists with `EligibilityLists`' parser (#305): doctor's own cases.
#[test]
fn identity_lists_that_cannot_be_trusted_are_refused() {
    let agents = crate::prerequisites_fakes::AGENTS;
    let many: Vec<String> = (1..=MAX_LIST_ENTRIES + 1)
        .map(|id| format!("{{ id = {id}, login = \"u{id}\" }}"))
        .collect();
    let too_many = owners_file(&many.join(", "));
    let unknown_field = owners_file("{ id = 1, login = \"a\", role = \"x\" }");
    let zero = owners_file("{ id = 0, login = \"a\" }");
    let no_login = owners_file("{ id = 1, login = \"\" }");
    let repeated_agent = "agents = [{ id = 7, login = \"a\" }, { id = 7, login = \"b\" }]";

    assert_refused(&[
        (&unknown_field, agents, owners_toml),
        ("admins = []", agents, owners_toml),
        (&zero, agents, owners_toml),
        (&no_login, agents, owner_login),
        (
            crate::prerequisites_fakes::OWNERS,
            repeated_agent,
            |error| matches!(error, ListsError::Repeated { list: ListName::Agents, id } if id.0.get() == 7),
        ),
        (&too_many, agents, |error| {
            matches!(
                error,
                ListsError::TooMany {
                    list: ListName::Owners
                }
            )
        }),
    ]);
}

/// #305: what every run refuses and doctor's former parser let through, so doctor passed it.
#[test]
fn identity_lists_a_run_refuses_are_refused_by_doctor_too() {
    let agents = crate::prerequisites_fakes::AGENTS;
    let owner_approver = "owners = [{ id = 1, login = \"a\" }]\napprovers = [{ id = 1, login = \"a\" }]\nmaintainers = []\n";
    let bot = owners_file("{ id = 1, login = \"a[bot]\" }");
    let hyphen = owners_file("{ id = 1, login = \"-a\" }");

    assert_refused(&[
        ("owners = []\nmaintainers = []\n", agents, owners_toml),
        (
            owner_approver,
            agents,
            |error| matches!(error, ListsError::OwnerApprover { id } if id.0.get() == 1),
        ),
        (&bot, agents, owner_login),
        (&hyphen, agents, owner_login),
    ]);
}
