//! `assay doctor --repository` (#75): its arguments, [`run_prerequisites`] and
//! [`write_prerequisites`] over a fake of the read port, and the binary's exit 2 until the
//! live transport (#273) exists.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;
use std::process::{Command, ExitCode};

use assay_app::{
    Access, AppId, AppliedRule, BranchPolicy, BypassActor, Collaborator, Conditions,
    DeploymentBranches, DirectoryEntry, Enforcement, EntryType, PolicyKind, PrerequisiteApps,
    PrerequisiteForge, RequiredCheck, RequiredChecks, Rule, Ruleset, RulesetId, RulesetTarget,
    UserId, WorkflowPermissions,
};
use assay_cli::{
    PrerequisitesArgs, PrerequisitesRunError, PrerequisitesUsage, prerequisites_exit_code,
    run_prerequisites, write_prerequisites,
};

const SHA: &str = "3d3c42e5aac5ba805825da76410c181273ba90b1";
const VERDICT_APP: u64 = 5_371_952;
const LEDGER_APP: u64 = 5_400_000;

fn nonzero(id: u64) -> NonZeroU64 {
    NonZeroU64::new(id).expect("test ids are not zero")
}

fn apps() -> PrerequisiteApps {
    PrerequisiteApps {
        verdict: AppId(nonzero(VERDICT_APP)),
        ledger: AppId(nonzero(LEDGER_APP)),
    }
}

fn args(text: &[&str]) -> Vec<OsString> {
    text.iter().map(OsString::from).collect()
}

/// The fake was told to fail.
#[derive(Debug)]
pub(crate) struct Refused;

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("refused")
    }
}

impl std::error::Error for Refused {}

/// One repository on `main`, files by path; everything else meets 05 §7.
#[derive(Debug, Clone, Default)]
pub(crate) struct Fake {
    pub(crate) files: BTreeMap<&'static str, String>,
    rulesets: Vec<Ruleset>,
    rules: Vec<AppliedRule>,
    collaborators: Vec<Collaborator>,
    agent_access: Option<Access>,
}

const OWNERS: &str = "owners = [{ id = 1001, login = \"break-glass\" }]\napprovers = [{ id = 1002, login = \"approver\" }]\nmaintainers = [{ id = 124330161, login = \"Ryan-Britton\" }]\n";
const AGENTS: &str = "agents = [{ id = 124330161, login = \"Ryan-Britton\" }]\n";

fn plugins(ledger: bool) -> String {
    let ledger = if ledger {
        "ledger_repo = \"Wet-Ink-Corporation/assay-ledger\"\n"
    } else {
        ""
    };
    format!(
        "[assay]\ndigest = \"sha256:{}\"\nprotected_bases = [\"main\"]\n{ledger}",
        "a".repeat(64)
    )
}

fn active(id: u64, target: RulesetTarget, rules: Vec<Rule>) -> Ruleset {
    Ruleset {
        id: RulesetId(nonzero(id)),
        name: format!("ruleset-{id}"),
        target,
        enforcement: Enforcement::Active,
        conditions: Conditions::default(),
        bypass: Vec::new(),
        rules,
    }
}

fn verdict_rule() -> Rule {
    Rule::RequiredStatusChecks(RequiredChecks {
        checks: vec![RequiredCheck {
            context: "assay / verdict".to_owned(),
            integration: Some(AppId(nonzero(VERDICT_APP))),
        }],
        strict: true,
    })
}

impl Fake {
    pub(crate) fn repository() -> Self {
        let mut files = BTreeMap::new();
        files.insert(".assay/plugins.toml", plugins(true));
        files.insert(".assay/owners", OWNERS.to_owned());
        files.insert(".assay/agents", AGENTS.to_owned());
        files.insert(
            ".github/workflows/assay.yml",
            format!(
                "on: pull_request_target\npermissions: {{}}\njobs:\n  review:\n    if: github.run_attempt == 1\n    environment: assay-review\n    steps:\n      - uses: actions/checkout@{SHA}\n"
            ),
        );
        let paths = [
            ".github/workflows/**",
            ".assay/**",
            ".claude/**",
            "CODEOWNERS",
            ".gitattributes",
        ];
        Self {
            files,
            rulesets: vec![
                active(10, RulesetTarget::Branch, vec![verdict_rule()]),
                active(
                    11,
                    RulesetTarget::Push,
                    vec![Rule::FilePathRestriction(paths.map(str::to_owned).to_vec())],
                ),
            ],
            rules: vec![AppliedRule {
                ruleset: RulesetId(nonzero(10)),
                rule: verdict_rule(),
            }],
            collaborators: Vec::new(),
            agent_access: Some(Access::Write),
        }
    }

    fn ledger() -> Self {
        Self {
            rulesets: vec![Ruleset {
                conditions: Conditions {
                    included: vec!["~ALL".to_owned()],
                    excluded: Vec::new(),
                    other: false,
                },
                bypass: vec![BypassActor::Integration(AppId(nonzero(LEDGER_APP)))],
                ..active(
                    30,
                    RulesetTarget::Branch,
                    vec![Rule::Creation, Rule::Update, Rule::Deletion],
                )
            }],
            collaborators: vec![Collaborator {
                user: UserId(nonzero(1001)),
                access: Access::Admin,
            }],
            agent_access: Some(Access::None),
            ..Self::default()
        }
    }
}

impl PrerequisiteForge for Fake {
    type Error = Refused;

    fn default_branch(&self) -> Result<String, Refused> {
        Ok("main".to_owned())
    }

    fn file(&self, branch: &str, path: &str) -> Result<Option<Vec<u8>>, Refused> {
        assert_eq!(branch, "main", "every file is read from the default branch");
        Ok(self.files.get(path).map(|text| text.as_bytes().to_vec()))
    }

    fn directory(&self, _: &str, path: &str) -> Result<Option<Vec<DirectoryEntry>>, Refused> {
        let prefix = format!("{path}/");
        let entries: Vec<_> = self
            .files
            .keys()
            .filter_map(|file| file.strip_prefix(&prefix))
            .map(|name| DirectoryEntry {
                name: name.to_owned(),
                kind: EntryType::File,
            })
            .collect();
        Ok(Some(entries))
    }

    fn rulesets(&self) -> Result<Vec<Ruleset>, Refused> {
        Ok(self.rulesets.clone())
    }

    fn branch_rules(&self, _: &str) -> Result<Vec<AppliedRule>, Refused> {
        Ok(self.rules.clone())
    }

    fn environment(&self, _: &str) -> Result<Option<DeploymentBranches>, Refused> {
        Ok(Some(DeploymentBranches::Custom(vec![BranchPolicy {
            name: "main".to_owned(),
            kind: PolicyKind::Branch,
        }])))
    }

    fn workflow_permissions(&self) -> Result<WorkflowPermissions, Refused> {
        Ok(WorkflowPermissions {
            can_approve_pull_requests: false,
        })
    }

    fn collaborators(&self) -> Result<Vec<Collaborator>, Refused> {
        Ok(self.collaborators.clone())
    }

    fn access(&self, _: UserId) -> Result<Access, Refused> {
        self.agent_access.clone().ok_or(Refused)
    }
}

/// What `write_prerequisites` wrote for `repository`, with the ledger [`Fake::ledger`].
pub(crate) fn written(repository: &Fake) -> (bool, String) {
    let report = run_prerequisites(repository, |_| Ok::<_, Refused>(Fake::ledger()), apps())
        .expect("base config is readable");
    let mut out = Vec::new();
    write_prerequisites(&report, &mut out).expect("a Vec takes every write");
    (
        report.passed(),
        String::from_utf8(out).expect("the report is UTF-8"),
    )
}

#[test]
fn the_arguments_name_the_repository_and_both_apps() {
    let parsed = PrerequisitesArgs::parse(args(&[
        "--ledger-app",
        "5400000",
        "--repository",
        "Wet-Ink-Corporation/assay",
        "--app",
        "5371952",
    ]))
    .expect("valid arguments");

    assert_eq!(parsed.repository.to_string(), "Wet-Ink-Corporation/assay");
    assert_eq!(parsed.apps, apps());
    assert!(PrerequisitesArgs::requested(Some(&OsString::from("--app"))));
    assert!(!PrerequisitesArgs::requested(Some(&OsString::from(
        "plugins.toml"
    ))));
    assert!(!PrerequisitesArgs::requested(None));
}

#[test]
fn arguments_that_do_not_name_all_three_are_refused() {
    let full = ["--repository", "o/r", "--app", "1", "--ledger-app", "2"];
    let cases: [(Vec<&str>, PrerequisitesUsage); 6] = [
        (vec!["--repo", "o/r"], PrerequisitesUsage::Unknown),
        (
            [&full[..], &["--app", "3"][..]].concat(),
            PrerequisitesUsage::Repeated("--app"),
        ),
        (
            vec!["--app", "1", "--ledger-app", "2"],
            PrerequisitesUsage::Missing("--repository"),
        ),
        (
            vec!["--repository", "not-a-repository"],
            PrerequisitesUsage::Value("--repository"),
        ),
        (
            vec!["--repository", "o/r", "--app", "0"],
            PrerequisitesUsage::Value("--app"),
        ),
        (
            vec!["--repository", "o/r", "--app", "1", "--ledger-app"],
            PrerequisitesUsage::Value("--ledger-app"),
        ),
    ];

    for (given, expected) in cases {
        assert_eq!(
            PrerequisitesArgs::parse(args(&given)),
            Err(expected),
            "{given:?}"
        );
    }
}

#[test]
fn a_repository_meeting_every_prerequisite_writes_ok_lines_and_passes() {
    let (passed, out) = written(&Fake::repository());

    assert!(passed, "{out}");
    assert_eq!(out.lines().count(), 14, "{out}");
    assert!(out.lines().all(|line| line.starts_with("ok ")), "{out}");
    assert!(
        out.starts_with("ok verdict-ruleset\nok base-digest\n"),
        "{out}"
    );
}

#[test]
fn a_failed_prerequisite_writes_its_reason_and_fails() {
    let mut repository = Fake::repository();
    repository.files.insert(
        ".assay/owners",
        "owners = [{ id = 1001, login = \"break-glass\" }]\napprovers = []\nmaintainers = []\n"
            .to_owned(),
    );

    let (passed, out) = written(&repository);

    assert!(!passed, "{out}");
    assert!(
        out.contains("\nFAIL approver: no approver is listed in .assay/owners (H-13)\n"),
        "{out}"
    );
}

#[test]
fn the_exit_code_is_0_when_every_prerequisite_holds_and_1_when_one_fails() {
    let ledger = |_: &str| Ok::<_, Refused>(Fake::ledger());
    let mut failing = Fake::repository();
    failing.agent_access = Some(Access::Admin);

    let passing = run_prerequisites(&Fake::repository(), ledger, apps()).expect("readable");
    let failed = run_prerequisites(&failing, ledger, apps()).expect("readable");

    assert_eq!(prerequisites_exit_code(&passing), ExitCode::SUCCESS);
    assert_eq!(prerequisites_exit_code(&failed), ExitCode::FAILURE);
}

#[test]
fn an_unreadable_read_is_written_with_its_cause() {
    let mut repository = Fake::repository();
    repository.agent_access = None;

    let (passed, out) = written(&repository);

    assert!(!passed, "{out}");
    assert!(
        out.contains("\nFAIL agent-access: GitHub could not be read: refused\n"),
        "{out}"
    );
}

#[test]
fn without_a_ledger_repo_the_ledger_is_never_connected_and_its_checks_fail() {
    let mut repository = Fake::repository();
    repository
        .files
        .insert(".assay/plugins.toml", plugins(false));

    let report = run_prerequisites(
        &repository,
        |_| -> Result<Fake, Refused> { panic!("no ledger is configured") },
        apps(),
    )
    .expect("base config is readable");
    let mut out = Vec::new();
    write_prerequisites(&report, &mut out).expect("a Vec takes every write");

    let out = String::from_utf8(out).expect("UTF-8");
    assert!(out.ends_with(
        "FAIL ledger-collaborators: no ledger_repo is configured\nFAIL ledger-ruleset: no ledger_repo is configured\nFAIL ledger-agents: no ledger_repo is configured\n"
    ), "{out}");
}

#[test]
fn base_config_or_a_ledger_that_cannot_be_read_is_an_error() {
    let mut repository = Fake::repository();
    repository.files.remove(".assay/agents");

    let missing = run_prerequisites(&repository, |_| Ok::<_, Refused>(Fake::ledger()), apps());

    let Err(error @ PrerequisitesRunError::Base(_)) = missing else {
        panic!("a missing .assay/agents is a base error: {missing:?}");
    };
    assert_eq!(
        error.to_string(),
        "base config: the default branch \"main\" has no .assay/agents"
    );

    let unconnected = run_prerequisites(&Fake::repository(), |_| Err::<Fake, _>(Refused), apps());
    assert!(
        matches!(unconnected, Err(PrerequisitesRunError::Ledger(Refused))),
        "{unconnected:?}"
    );
}

#[test]
fn the_binary_stops_with_exit_2_until_the_live_transport_exists() {
    let output = Command::new(env!("CARGO_BIN_EXE_assay"))
        .args([
            "doctor",
            "--repository",
            "Wet-Ink-Corporation/assay",
            "--app",
            "5371952",
            "--ledger-app",
            "1",
        ])
        .env_clear()
        .output()
        .expect("the binary runs");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8");
    assert!(stderr.contains("#273"), "{stderr}");
}

#[test]
fn the_binary_refuses_bad_arguments_with_exit_2() {
    let output = Command::new(env!("CARGO_BIN_EXE_assay"))
        .args(["doctor", "--repository", "o/r"])
        .env_clear()
        .output()
        .expect("the binary runs");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("UTF-8");
    assert_eq!(
        stderr,
        "assay: --app is required; usage: assay doctor --repository <owner/name> --app <id> --ledger-app <id>\n"
    );
}
