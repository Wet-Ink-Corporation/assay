//! A fake of what doctor's repository checks read (TST-09): a repository and a ledger whose
//! settings meet every prerequisite of 05 §7, for each test to break one.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use assay_app::{
    Access, AppId, AppliedRule, BranchPolicy, BypassActor, Collaborator, Conditions,
    DeploymentBranches, DirectoryEntry, Enforcement, EntryType, PolicyKind, PrerequisiteApps,
    PrerequisiteForge, RequiredCheck, RequiredChecks, Rule, Ruleset, RulesetId, RulesetTarget,
    UserId, WorkflowPermissions,
};

use crate::plan_fakes::FakeError;

/// The default branch.
pub(crate) const MAIN: &str = "main";
/// The other protected base.
pub(crate) const RELEASE: &str = "release";
/// A full commit SHA, for pins.
pub(crate) const SHA: &str = "3d3c42e5aac5ba805825da76410c181273ba90b1";
/// The digest both bases pin.
pub(crate) const DIGEST: &str =
    "sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b";

/// The break-glass account, the only owner.
pub(crate) const OWNER: u64 = 1001;
/// The approver account.
pub(crate) const APPROVER: u64 = 1002;
/// The owner's daily account: a maintainer and the agent identity (docs/decisions/0005).
pub(crate) const DAILY: u64 = 124_330_161;

/// The ids of the verdict App and the ledger App.
pub(crate) fn apps() -> PrerequisiteApps {
    PrerequisiteApps {
        verdict: app(5_371_952),
        ledger: app(5_400_000),
    }
}

/// An app id.
pub(crate) fn app(id: u64) -> AppId {
    AppId(NonZeroU64::new(id).expect("test app ids are not zero"))
}

/// A user id.
pub(crate) fn user(id: u64) -> UserId {
    UserId(NonZeroU64::new(id).expect("test user ids are not zero"))
}

/// A ruleset id.
pub(crate) fn ruleset_id(id: u64) -> RulesetId {
    RulesetId(NonZeroU64::new(id).expect("test ruleset ids are not zero"))
}

/// `.assay/plugins.toml` pinning `digest`.
pub(crate) fn plugins(digest: &str) -> String {
    format!(
        "[assay]\ndigest = \"{digest}\"\nprotected_bases = [\"{MAIN}\", \"{RELEASE}\"]\nledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n"
    )
}

/// `.assay/owners` with one owner, one approver and the daily account as maintainer.
pub(crate) const OWNERS: &str = r#"
owners = [{ id = 1001, login = "assay-break-glass" }]
approvers = [{ id = 1002, login = "assay-approver" }]
maintainers = [{ id = 124330161, login = "Ryan-Britton" }]
"#;

/// `.assay/agents` naming the daily account.
pub(crate) const AGENTS: &str = r#"agents = [{ id = 124330161, login = "Ryan-Britton" }]"#;

/// The installed review workflow: default-branch triggers, a guarded privileged job.
pub(crate) fn assay_yml() -> String {
    format!(
        "on:\n  pull_request_target:\n    types: [opened, synchronize]\npermissions: {{}}\njobs:\n  review:\n    if: github.run_attempt == 1\n    environment: assay-review\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@{SHA}\n  plan:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo plan\n"
    )
}

/// The installed decide-only workflow.
pub(crate) const ASSAY_DECIDE_YML: &str = "on: [issue_comment, schedule]\npermissions:\n  checks: write\njobs:\n  decide:\n    if: always() && github.run_attempt == 1\n    runs-on: ubuntu-latest\n";

/// A workflow that is not assay's: `pull_request` is allowed there, pins are not optional.
pub(crate) fn rust_yml() -> String {
    format!(
        "on: [push, pull_request]\npermissions:\n  contents: read\njobs:\n  gate:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@{SHA}\n"
    )
}

/// A required verdict check rule of ruleset `ruleset` for `context`.
pub(crate) fn verdict_rule(ruleset: u64, context: &str, strict: bool) -> AppliedRule {
    AppliedRule {
        ruleset: ruleset_id(ruleset),
        rule: Rule::RequiredStatusChecks(RequiredChecks {
            checks: vec![RequiredCheck {
                context: context.to_owned(),
                integration: Some(apps().verdict),
            }],
            strict,
        }),
    }
}

/// An active ruleset with no bypass actor.
pub(crate) fn ruleset(id: u64, target: RulesetTarget, rules: Vec<Rule>) -> Ruleset {
    Ruleset {
        id: ruleset_id(id),
        name: format!("ruleset-{id}"),
        target,
        enforcement: Enforcement::Active,
        conditions: Conditions::default(),
        bypass: Vec::new(),
        rules,
    }
}

/// The ledger's ruleset: every branch, restricted to the ledger App.
pub(crate) fn ledger_ruleset() -> Ruleset {
    Ruleset {
        conditions: Conditions {
            included: vec!["~ALL".to_owned()],
            excluded: Vec::new(),
            other: false,
        },
        bypass: vec![BypassActor::Integration(apps().ledger)],
        ..ruleset(
            30,
            RulesetTarget::Branch,
            vec![
                Rule::Creation,
                Rule::Update,
                Rule::Deletion,
                Rule::Other("non_fast_forward".to_owned()),
            ],
        )
    }
}

/// One repository's settings, as the fake answers them.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeRepository {
    pub(crate) default_branch: String,
    pub(crate) files: BTreeMap<(String, String), Vec<u8>>,
    pub(crate) directories: BTreeMap<(String, String), Vec<DirectoryEntry>>,
    pub(crate) rulesets: Vec<Ruleset>,
    pub(crate) branch_rules: BTreeMap<String, Vec<AppliedRule>>,
    pub(crate) environments: BTreeMap<String, DeploymentBranches>,
    pub(crate) can_approve: bool,
    pub(crate) collaborators: Vec<Collaborator>,
    pub(crate) access: BTreeMap<UserId, Access>,
    /// Reads that fail, by method name.
    pub(crate) broken: BTreeSet<&'static str>,
}

impl FakeRepository {
    /// A repository that meets every prerequisite.
    pub(crate) fn healthy() -> Self {
        let mut repository = Self {
            default_branch: MAIN.to_owned(),
            ..Self::default()
        };
        repository.put(MAIN, ".assay/plugins.toml", &plugins(DIGEST));
        repository.put(RELEASE, ".assay/plugins.toml", &plugins(DIGEST));
        repository.put(MAIN, ".assay/owners", OWNERS);
        repository.put(MAIN, ".assay/agents", AGENTS);
        repository.workflow("assay.yml", &assay_yml());
        repository.workflow("assay-decide.yml", ASSAY_DECIDE_YML);
        repository.workflow("rust.yml", &rust_yml());
        repository.rulesets = vec![
            ruleset(
                10,
                RulesetTarget::Branch,
                vec![verdict_rule(10, "assay / verdict", true).rule],
            ),
            ruleset(
                11,
                RulesetTarget::Push,
                vec![Rule::FilePathRestriction(
                    [
                        ".github/workflows/**",
                        ".assay/**",
                        ".claude/**",
                        "CODEOWNERS",
                        ".gitattributes",
                    ]
                    .map(str::to_owned)
                    .to_vec(),
                )],
            ),
            ruleset(
                12,
                RulesetTarget::Branch,
                vec![verdict_rule(12, "assay / verdict @release", true).rule],
            ),
        ];
        repository.branch_rules.insert(
            MAIN.to_owned(),
            vec![verdict_rule(10, "assay / verdict", true)],
        );
        repository.branch_rules.insert(
            RELEASE.to_owned(),
            vec![verdict_rule(12, "assay / verdict @release", true)],
        );
        for name in ["assay-publish", "assay-review"] {
            repository.environments.insert(
                name.to_owned(),
                DeploymentBranches::Custom(vec![BranchPolicy {
                    name: MAIN.to_owned(),
                    kind: PolicyKind::Branch,
                }]),
            );
        }
        repository.access.insert(user(DAILY), Access::Write);
        repository
    }

    /// The ledger repository, meeting every ledger prerequisite.
    pub(crate) fn healthy_ledger() -> Self {
        let mut ledger = Self {
            default_branch: MAIN.to_owned(),
            rulesets: vec![ledger_ruleset()],
            collaborators: vec![Collaborator {
                user: user(OWNER),
                access: Access::Admin,
            }],
            ..Self::default()
        };
        ledger.access.insert(user(DAILY), Access::None);
        ledger
    }

    /// Puts `text` at `path` on `branch`.
    pub(crate) fn put(&mut self, branch: &str, path: &str, text: &str) {
        self.files.insert(
            (branch.to_owned(), path.to_owned()),
            text.as_bytes().to_vec(),
        );
    }

    /// Puts workflow `name` on the default branch and lists it.
    pub(crate) fn workflow(&mut self, name: &str, text: &str) {
        self.put(MAIN, &format!(".github/workflows/{name}"), text);
        self.directories
            .entry((MAIN.to_owned(), ".github/workflows".to_owned()))
            .or_default()
            .push(DirectoryEntry {
                name: name.to_owned(),
                kind: EntryType::File,
            });
    }

    fn read(&self, method: &'static str) -> Result<(), FakeError> {
        if self.broken.contains(method) {
            Err(FakeError)
        } else {
            Ok(())
        }
    }
}

impl PrerequisiteForge for FakeRepository {
    type Error = FakeError;

    fn default_branch(&self) -> Result<String, FakeError> {
        self.read("default_branch")?;
        Ok(self.default_branch.clone())
    }

    fn file(&self, branch: &str, path: &str) -> Result<Option<Vec<u8>>, FakeError> {
        self.read("file")?;
        Ok(self
            .files
            .get(&(branch.to_owned(), path.to_owned()))
            .cloned())
    }

    fn directory(
        &self,
        branch: &str,
        path: &str,
    ) -> Result<Option<Vec<DirectoryEntry>>, FakeError> {
        self.read("directory")?;
        Ok(self
            .directories
            .get(&(branch.to_owned(), path.to_owned()))
            .cloned())
    }

    fn rulesets(&self) -> Result<Vec<Ruleset>, FakeError> {
        self.read("rulesets")?;
        Ok(self.rulesets.clone())
    }

    fn branch_rules(&self, branch: &str) -> Result<Vec<AppliedRule>, FakeError> {
        self.read("branch_rules")?;
        Ok(self.branch_rules.get(branch).cloned().unwrap_or_default())
    }

    fn environment(&self, name: &str) -> Result<Option<DeploymentBranches>, FakeError> {
        self.read("environment")?;
        Ok(self.environments.get(name).cloned())
    }

    fn workflow_permissions(&self) -> Result<WorkflowPermissions, FakeError> {
        self.read("workflow_permissions")?;
        Ok(WorkflowPermissions {
            can_approve_pull_requests: self.can_approve,
        })
    }

    fn collaborators(&self) -> Result<Vec<Collaborator>, FakeError> {
        self.read("collaborators")?;
        Ok(self.collaborators.clone())
    }

    fn access(&self, user: UserId) -> Result<Access, FakeError> {
        self.read("access")?;
        Ok(self.access.get(&user).cloned().unwrap_or(Access::None))
    }
}
