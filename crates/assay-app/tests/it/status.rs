//! `assay status` (#126, 03 §6b): the golden `assay.status/1` documents of
//! `schemas/fixtures/status/valid/`, which the mod's tests import, each produced here from what
//! `publish` wrote, and the behaviours they pin.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::num::NonZeroU64;

use assay_app::{
    CommitId, ContractValidator, Detail, Identities, ObservedConclusion, PullSelector, StatusError,
    StatusPorts, StatusReport, StatusRepository, StatusRequest, status,
};
use assay_domain::{
    Deferral, Disposition, FindingDisposition, GateConclusion, GateFinding, GateOutcome,
    GateReport, LlmStage, OwnerApproval, PrClass, ReviewedFinding, Rubric, Severity,
    StepAttribution, VerifierVerdict,
};
use serde_json::Value;

use crate::plan_fakes::{FakeError, HEAD};
use crate::publish_support::{Run, stamped};
use crate::status_fakes::{BRANCH, FETCHED_AT, FakeGitHub, REPO, app, other_app, run_url, user};
use crate::support::{rubric_v1, schemas_dir};

/// The owner's privileged id and an agent's id in the base lists.
const OWNER: u64 = 1;
const AGENT: u64 = 2;

/// A local HEAD other than the PR's.
const OTHER_HEAD: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// The published comment of the reviewer finding `00000000000000ab`, as `publish_findings`
/// renders it.
pub(crate) const COMMENT: &str = "<!-- assay-finding 00000000000000ab -->\n\
     🔴 **blocker** · logic · defect · confidence high\n\
     \n\
     **Event recorded twice**\n\
     \n\
     The retry path records the event again\\.\n\
     \n\
     ```rust\n\
     record_event(id);\n\
     ```\n\
     \n\
     React with 👍 if this finding is useful or 👎 if it is not. Fingerprint \
     `00000000000000ab`; an eligible owner can dismiss it with \
     `/assay dismiss 00000000000000ab <reason>`.";

/// What a test asks `status`.
pub(crate) struct Ask {
    pub(crate) pull: PullSelector<'static>,
    pub(crate) local: Option<CommitId>,
    pub(crate) detail: Detail,
}

impl Ask {
    /// PR 42 by number, no local checkout, without bodies.
    pub(crate) const fn new() -> Self {
        Self {
            pull: PullSelector::Number(NonZeroU64::MIN.saturating_add(41)),
            local: None,
            detail: Detail::Summary,
        }
    }
}

/// `status` over `github` as `ask` says, for the repository whose base lists make user 1 the
/// owner and user 2 an agent.
pub(crate) fn report(
    github: &FakeGitHub,
    ask: &Ask,
) -> Result<Result<StatusReport, StatusError<FakeError, FakeError>>, Box<dyn Error>> {
    report_with(github, ask, &rubric_v1()?)
}

/// [`report`] under `rubric`.
pub(crate) fn report_with(
    github: &FakeGitHub,
    ask: &Ask,
    rubric: &Rubric,
) -> Result<Result<StatusReport, StatusError<FakeError, FakeError>>, Box<dyn Error>> {
    report_as(github, ask, rubric, Place::new(REPO, FETCHED_AT))
}

/// [`report`] for a repository named `name`.
pub(crate) fn report_named(
    github: &FakeGitHub,
    name: &str,
) -> Result<Result<StatusReport, StatusError<FakeError, FakeError>>, Box<dyn Error>> {
    report_as(
        github,
        &Ask::new(),
        &rubric_v1()?,
        Place::new(name, FETCHED_AT),
    )
}

/// [`report`] read at `fetched_at`.
pub(crate) fn report_fetched_at(
    github: &FakeGitHub,
    fetched_at: &str,
) -> Result<Result<StatusReport, StatusError<FakeError, FakeError>>, Box<dyn Error>> {
    report_as(
        github,
        &Ask::new(),
        &rubric_v1()?,
        Place::new(REPO, fetched_at),
    )
}

/// The repository's name and the time the state is read at.
#[derive(Clone, Copy)]
struct Place<'a> {
    name: &'a str,
    fetched_at: &'a str,
}

impl<'a> Place<'a> {
    const fn new(name: &'a str, fetched_at: &'a str) -> Self {
        Self { name, fetched_at }
    }
}

fn report_as(
    github: &FakeGitHub,
    ask: &Ask,
    rubric: &Rubric,
    place: Place<'_>,
) -> Result<Result<StatusReport, StatusError<FakeError, FakeError>>, Box<dyn Error>> {
    let repository = StatusRepository {
        id: NonZeroU64::new(840_213_377).ok_or("not zero")?,
        name: place.name.to_owned(),
        default_branch: "main".to_owned(),
    };
    let identities = Identities {
        privileged: BTreeSet::from([user(OWNER)]),
        agents: BTreeSet::from([user(AGENT)]),
    };
    let request = StatusRequest {
        repository: &repository,
        app: app(),
        pull: ask.pull,
        local_head: ask.local.as_ref(),
        detail: ask.detail,
        rubric,
        identities: &identities,
        fetched_at: place.fetched_at,
    };
    let ports = StatusPorts {
        forge: github,
        bundles: github,
    };
    Ok(status(&request, &ports, &ContractValidator::new()?))
}

/// The status document `github` gives for `ask`, parsed.
pub(crate) fn document(github: &FakeGitHub, ask: &Ask) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(report(github, ask)??.json())?)
}

/// The golden status document `name`.
fn golden(name: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let path = schemas_dir()
        .join("fixtures/status/valid")
        .join(format!("{name}.json"));
    Ok(fs::read(path)?)
}

/// Asserts `github` gives exactly the golden `name` for `ask`.
fn assert_golden(github: &FakeGitHub, ask: &Ask, name: &str) -> Result<(), Box<dyn Error>> {
    let json = report(github, ask)??.json().to_vec();
    let expected = golden(name)?;
    assert_eq!(
        String::from_utf8(json)?,
        String::from_utf8(expected)?,
        "golden {name}"
    );
    Ok(())
}

pub(crate) fn fp(text: &str) -> Result<assay_domain::Fingerprint, Box<dyn Error>> {
    Ok(text.parse()?)
}

/// A verified, advisory reviewer blocker `00000000000000ab` from `combined-micro`.
fn reviewer_blocker() -> Result<ReviewedFinding, Box<dyn Error>> {
    Ok(ReviewedFinding {
        fp: fp("00000000000000ab")?,
        group_id: "g1".to_owned(),
        role: "combined-micro".to_owned(),
        severity: Severity::Blocker,
        pre_existing: false,
        verdict: Some(VerifierVerdict::Confirmed),
    })
}

/// The default run with one published reviewer blocker, decided `success` (advisory).
pub(crate) fn success_world() -> Result<FakeGitHub, Box<dyn Error>> {
    let mut run = Run::new()?;
    run.inputs.findings = vec![reviewer_blocker()?];
    run.inputs.dispositions = vec![FindingDisposition {
        fp: fp("00000000000000ab")?,
        code: Disposition::LineNotInDiffMoved,
    }];
    let world = stamped()?;
    run.publish(&world)?;
    let mut github = FakeGitHub::published(&world)
        .with_gate("temper", ObservedConclusion::Success, 9101)
        .with_gate("custom-lint", ObservedConclusion::Success, 9102)
        .with_comment(COMMENT, ("src/lib.rs", 12), 501)
        .viewed_by(OWNER, "ryan-britton");
    github
        .checks
        .push(forged_verdict(ObservedConclusion::Failure));
    Ok(github)
}

/// A failing `temper` gate with a raised baseline (waivable) and a clippy blocker (never),
/// beside the reviewer blocker.
pub(crate) fn failure_world() -> Result<FakeGitHub, Box<dyn Error>> {
    let mut run = Run::new()?;
    run.inputs.gates = vec![
        GateReport {
            name: "temper".to_owned(),
            required: true,
            outcome: GateOutcome::Ran {
                conclusion: GateConclusion::Fail,
                steps: StepAttribution::Unknown,
            },
            findings: vec![
                GateFinding {
                    fp: fp("00000000000000a1")?,
                    rule: Some("raised".to_owned()),
                    severity: Severity::Blocker,
                },
                GateFinding {
                    fp: fp("00000000000000a2")?,
                    rule: Some("clippy::unwrap_used".to_owned()),
                    severity: Severity::Blocker,
                },
            ],
        },
        GateReport {
            name: "custom-lint".to_owned(),
            required: false,
            outcome: GateOutcome::Ran {
                conclusion: GateConclusion::Pass,
                steps: StepAttribution::Unknown,
            },
            findings: Vec::new(),
        },
    ];
    run.inputs.findings = vec![reviewer_blocker()?];
    let world = stamped()?;
    run.publish(&world)?;
    Ok(FakeGitHub::published(&world)
        .with_gate("temper", ObservedConclusion::Failure, 9101)
        .with_gate("custom-lint", ObservedConclusion::Success, 9102)
        .with_comment(COMMENT, ("src/lib.rs", 12), 501)
        .viewed_by(AGENT, "claude-agent"))
}

/// A trust-path PR, approved, whose LLM stage the budget deferred: `pending`.
fn pending_world() -> Result<FakeGitHub, Box<dyn Error>> {
    let mut run = Run::new()?;
    run.inputs.pr.classes = vec![PrClass::TrustPath];
    run.inputs.pr.owner_approval = OwnerApproval::Approved;
    run.inputs.llm = LlmStage::Deferred {
        reason: Deferral::BudgetDeferred,
    };
    run.inputs.completed_roles = BTreeSet::new();
    let world = stamped()?;
    run.publish(&world)?;
    Ok(FakeGitHub::published(&world))
}

/// A check named `assay / verdict` that a PR's workflow posted as GitHub Actions.
pub(crate) fn forged_verdict(conclusion: ObservedConclusion) -> assay_app::CheckView {
    assay_app::CheckView {
        app: other_app(),
        name: "assay / verdict".to_owned(),
        status: assay_app::CheckStatus::Completed,
        conclusion: Some(conclusion),
        external_id: Some(
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_owned(),
        ),
        output_text: Some("anything".to_owned()),
        url: run_url(666),
    }
}

// --- the goldens ---

#[test]
fn a_successful_verdict_matches_its_golden() -> Result<(), Box<dyn Error>> {
    let ask = Ask {
        local: Some(HEAD.parse()?),
        ..Ask::new()
    };

    assert_golden(&success_world()?, &ask, "success")
}

#[test]
fn a_failure_with_blockers_matches_its_golden() -> Result<(), Box<dyn Error>> {
    let ask = Ask {
        local: Some(OTHER_HEAD.parse()?),
        detail: Detail::Full,
        ..Ask::new()
    };

    assert_golden(&failure_world()?, &ask, "failure-with-blockers")
}

#[test]
fn a_run_in_progress_matches_its_golden() -> Result<(), Box<dyn Error>> {
    let world = stamped()?;

    assert_golden(&FakeGitHub::published(&world), &Ask::new(), "in-progress")
}

#[test]
fn a_budget_deferred_pending_verdict_matches_its_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(&pending_world()?, &Ask::new(), "pending-budget-deferred")
}

#[test]
fn a_pr_without_a_check_matches_its_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(&FakeGitHub::new(), &Ask::new(), "no-check")
}

#[test]
fn a_forged_same_named_check_from_another_app_matches_its_golden() -> Result<(), Box<dyn Error>> {
    let mut github = FakeGitHub::new();
    github
        .checks
        .push(forged_verdict(ObservedConclusion::Success));

    assert_golden(&github, &Ask::new(), "forged-check-ignored")
}

// --- what the goldens pin ---

#[test]
fn a_forged_check_beside_the_apps_never_replaces_it() -> Result<(), Box<dyn Error>> {
    let status = document(&success_world()?, &Ask::new())?;

    assert_eq!(
        status.pointer("/check/url"),
        Some(&Value::from(run_url(9001))),
        "the App's check, not the forged failure at run 666"
    );
    Ok(())
}

#[test]
fn waivable_is_true_for_a_raised_baseline_and_false_for_clippy_and_reviewer_findings()
-> Result<(), Box<dyn Error>> {
    let status = document(&failure_world()?, &Ask::new())?;

    let waivable: Vec<(Option<&str>, Option<bool>)> = status["findings"]
        .as_array()
        .ok_or("findings")?
        .iter()
        .map(|finding| (finding["fp"].as_str(), finding["waivable"].as_bool()))
        .collect();
    assert_eq!(
        waivable,
        [
            (Some("00000000000000a1"), Some(true)),
            (Some("00000000000000a2"), Some(false)),
            (Some("00000000000000ab"), Some(false)),
        ]
    );
    Ok(())
}

#[test]
fn the_reasons_digest_is_the_one_override_quotes() -> Result<(), Box<dyn Error>> {
    let status = document(&failure_world()?, &Ask::new())?;

    // `printf 'gate-failed\n' | sha256sum` (04 §5).
    assert_eq!(
        status.pointer("/decision/reasons_digest"),
        Some(&Value::from(
            "sha256:edfab635b96543711e1a2662a111f30252334d5a72e82c9d11bb62e35ad7e231"
        ))
    );
    Ok(())
}

#[test]
fn the_viewer_is_owner_agent_or_other_by_numeric_id() -> Result<(), Box<dyn Error>> {
    let cases = [
        (OWNER, "ryan-britton", (true, false)),
        (AGENT, "claude-agent", (false, true)),
        (3, "someone", (false, false)),
        (OWNER, "renamed-owner", (true, false)),
    ];

    for (id, login, expected) in cases {
        let github = FakeGitHub::new().viewed_by(id, login);
        let status = document(&github, &Ask::new())?;
        let viewer = &status["viewer"];
        assert_eq!(viewer["login"].as_str(), Some(login));
        assert_eq!(
            (viewer["is_owner"].as_bool(), viewer["is_agent"].as_bool()),
            (Some(expected.0), Some(expected.1)),
            "viewer {id} {login}"
        );
    }
    Ok(())
}

#[test]
fn stale_says_whether_the_local_head_differs_from_the_checks() -> Result<(), Box<dyn Error>> {
    let cases = [(None, false), (Some(HEAD), false), (Some(OTHER_HEAD), true)];

    for (local, stale) in cases {
        let ask = Ask {
            local: local.map(str::parse).transpose()?,
            ..Ask::new()
        };
        let status = document(&FakeGitHub::new(), &ask)?;
        assert_eq!(status["stale"].as_bool(), Some(stale), "local {local:?}");
    }
    Ok(())
}

#[test]
fn without_full_no_body_is_shown() -> Result<(), Box<dyn Error>> {
    let status = document(&failure_world()?, &Ask::new())?;

    assert_eq!(status.pointer("/findings/2/body"), None);
    assert_eq!(
        status.pointer("/findings/2/title"),
        Some(&Value::from("Event recorded twice"))
    );
    Ok(())
}

#[test]
fn the_branch_selects_its_one_open_pr() -> Result<(), Box<dyn Error>> {
    let ask = Ask {
        pull: PullSelector::Branch(BRANCH),
        ..Ask::new()
    };

    let status = document(&FakeGitHub::new(), &ask)?;

    assert_eq!(status["pr"].as_u64(), Some(42));
    Ok(())
}

#[test]
fn the_text_states_the_same_facts() -> Result<(), Box<dyn Error>> {
    let ask = Ask {
        local: Some(OTHER_HEAD.parse()?),
        ..Ask::new()
    };

    let report = report(&failure_world()?, &ask)??;

    assert_eq!(
        report.text(),
        format!(
            "assay status: {REPO} #42 ({BRANCH})\n\
             Head: {HEAD} (local HEAD differs: this status is for another commit)\n\
             Check: completed failure https://github.com/{REPO}/runs/9001\n\
             Decision: failure (tier micro)\n\
             \x20 reason gate-failed: temper\n\
             \x20 counts: 3 blocker, 0 major, 0 minor, 0 nit\n\
             \x20 advisory blocker 00000000000000ab\n\
             \x20 reasons digest: \
             sha256:edfab635b96543711e1a2662a111f30252334d5a72e82c9d11bb62e35ad7e231\n\
             Gates:\n\
             \x20 custom-lint success https://github.com/{REPO}/runs/9102\n\
             \x20 temper failure https://github.com/{REPO}/runs/9101\n\
             Findings:\n\
             \x20 00000000000000a1 blocker waivable temper: raised\n\
             \x20 00000000000000a2 blocker temper: clippy::unwrap_used\n\
             \x20 00000000000000ab blocker logic src/lib.rs:12 Event recorded twice\n\
             Viewer: claude-agent (agent)\n\
             Fetched: {FETCHED_AT}\n"
        )
    );
    Ok(())
}
