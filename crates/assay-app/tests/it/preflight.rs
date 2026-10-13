//! The preflight of the LLM stage (06 §1, 03 §8, #83): no credential- or provider-selecting
//! variable in the job's environment, `claude auth status` proving an OAuth first-party session on
//! the pinned CLI, and a clean `system/init` on the first call. Every failure is the
//! LLM-unavailable policy.

use std::convert::Infallible;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;

use assay_app::{
    AuthProof, BuiltinKind, CliProbe, CliStatus, Contract, ContractValidator, InitParts,
    InitReadError, InitUnclean, MAX_BUILTIN_NAME_BYTES, PreflightFailed, PreflightFailure,
    SessionInit, StatusParts, StatusReadError, TOKEN_ROTATION_RUNBOOK, UNOBSERVED,
    is_provider_variable, preflight,
};
use assay_domain::{BuiltinSet, Deferral, Event, EventPayload, EventSchema, Preflight, RunUid};

use crate::session_init::{observed, pinned};

/// Why the fake probe could not run the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProbeDown;

impl fmt::Display for ProbeDown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the CLI could not be started")
    }
}

impl Error for ProbeDown {}

/// A probe answering with a fixed status, or failing.
struct FakeProbe(Result<CliStatus, ProbeDown>);

impl CliProbe for FakeProbe {
    type Error = ProbeDown;

    fn status(&self) -> Result<CliStatus, ProbeDown> {
        self.0.clone()
    }
}

/// What 2.1.296 reports for a `CLAUDE_CODE_OAUTH_TOKEN` session.
fn oauth_parts() -> StatusParts {
    StatusParts {
        version: "2.1.296".to_owned(),
        logged_in: true,
        auth_method: "oauth_token".to_owned(),
        api_provider: "firstParty".to_owned(),
    }
}

fn probe(parts: StatusParts) -> Result<FakeProbe, Box<dyn Error>> {
    Ok(FakeProbe(Ok(CliStatus::new(parts)?)))
}

/// The variables of an ordinary review job.
const CLEAN_ENV: [&str; 4] = ["HOME", "PATH", "CLAUDE_CODE_OAUTH_TOKEN", "GITHUB_ACTIONS"];

/// Phase one against `probe` in a clean environment.
fn proof_with(
    probe: &FakeProbe,
) -> Result<Result<AuthProof, PreflightFailed<ProbeDown>>, Box<dyn Error>> {
    let (version, _) = pinned()?;
    Ok(preflight(CLEAN_ENV, probe, &version))
}

fn proof() -> Result<AuthProof, Box<dyn Error>> {
    proof_with(&probe(oauth_parts())?)?.map_err(|failed| format!("{failed:?}").into())
}

/// The failure of phase one with `parts` as the status, in a clean environment.
fn failure_with(parts: StatusParts) -> Result<PreflightFailed<ProbeDown>, Box<dyn Error>> {
    match proof_with(&probe(parts)?)? {
        Ok(proof) => Err(format!("unexpected proof {proof:?}").into()),
        Err(failed) => Ok(failed),
    }
}

/// The failure of phase two on `init`.
fn first_call_failure(
    init: Result<SessionInit, InitReadError>,
) -> Result<PreflightFailed<Infallible>, Box<dyn Error>> {
    let (_, allowlist) = pinned()?;
    match proof()?.check_first_call(init, &allowlist) {
        Ok(passed) => Err(format!("unexpected pass {passed:?}").into()),
        Err(failed) => Ok(failed),
    }
}

fn unobserved() -> Preflight {
    Preflight {
        auth_method: UNOBSERVED.to_owned(),
        cli_version: UNOBSERVED.to_owned(),
        init_clean: false,
        builtins: None,
    }
}

fn observed_set() -> BuiltinSet {
    let parts = observed();
    BuiltinSet {
        plugins: parts
            .plugins
            .into_iter()
            .map(|plugin| plugin.name)
            .collect(),
        skills: parts.skills,
        agents: parts.agents,
    }
}

#[test]
fn a_clean_environment_and_an_oauth_status_prove_auth() -> Result<(), Box<dyn Error>> {
    let proof = proof()?;

    assert_eq!(
        (proof.auth_method(), proof.cli_version()),
        ("oauth_token", "2.1.296")
    );
    Ok(())
}

#[test]
fn a_clean_first_init_passes_with_the_observed_builtins() -> Result<(), Box<dyn Error>> {
    let (_, allowlist) = pinned()?;

    let passed = proof()?
        .check_first_call::<Infallible>(Ok(SessionInit::new(observed())?), &allowlist)
        .map_err(|failed| format!("{failed:?}"))?;

    assert_eq!(
        passed.into_event(),
        EventPayload::PreflightPassed(Preflight {
            auth_method: "oauth_token".to_owned(),
            cli_version: "2.1.296".to_owned(),
            init_clean: true,
            builtins: Some(observed_set()),
        })
    );
    Ok(())
}

#[test]
fn each_credential_or_provider_variable_fails_even_when_empty() -> Result<(), Box<dyn Error>> {
    let (version, _) = pinned()?;
    let probe = probe(oauth_parts())?;
    let names = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "AWS_BEARER_TOKEN_BEDROCK",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "CLOUD_ML_REGION",
        "VERTEX_REGION_CLAUDE_4_0_SONNET",
    ];
    for name in names {
        let variables = ["HOME", name, "PATH"];

        let failed = preflight(variables, &probe, &version).map_err(PreflightFailed::into_parts);

        assert_eq!(
            failed.err(),
            Some((
                EventPayload::PreflightFailed(unobserved()),
                PreflightFailure::ProviderVariable {
                    name: name.to_owned()
                }
            )),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn a_non_utf8_variable_with_a_provider_prefix_fails() -> Result<(), Box<dyn Error>> {
    let (version, _) = pinned()?;
    let name = OsStr::from_bytes(b"ANTHROPIC_\xff");

    let failed =
        preflight([name], &probe(oauth_parts())?, &version).map_err(PreflightFailed::into_parts);

    assert_eq!(
        failed.err().map(|(_, reason)| reason),
        Some(PreflightFailure::ProviderVariable {
            name: "ANTHROPIC_\u{fffd}".to_owned()
        })
    );
    Ok(())
}

#[test]
fn names_that_only_resemble_a_provider_prefix_are_not_provider_variables() {
    let names = [
        "ANTHROPIC",
        "anthropic_api_key",
        "MY_ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "AWS",
    ];

    let flagged: Vec<&str> = names
        .into_iter()
        .filter(|name| is_provider_variable(OsStr::new(name)))
        .collect();

    assert_eq!(flagged, Vec::<&str>::new());
}

#[test]
fn a_probe_that_cannot_run_fails_with_nothing_observed() -> Result<(), Box<dyn Error>> {
    let failed = proof_with(&FakeProbe(Err(ProbeDown)))?.map_err(PreflightFailed::into_parts);

    assert_eq!(
        failed.err(),
        Some((
            EventPayload::PreflightFailed(unobserved()),
            PreflightFailure::Probe(ProbeDown)
        ))
    );
    Ok(())
}

#[test]
fn another_cli_version_than_the_pin_fails() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        version: "2.1.297".to_owned(),
        ..oauth_parts()
    })?;

    assert_eq!(
        failed.into_parts(),
        (
            EventPayload::PreflightFailed(Preflight {
                auth_method: "oauth_token".to_owned(),
                cli_version: "2.1.297".to_owned(),
                ..unobserved()
            }),
            PreflightFailure::CliVersion {
                found: "2.1.297".to_owned(),
                pinned: "2.1.296".to_owned(),
            }
        )
    );
    Ok(())
}

#[test]
fn a_session_that_is_not_logged_in_fails() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        logged_in: false,
        auth_method: "none".to_owned(),
        ..oauth_parts()
    })?;

    assert_eq!(failed.reason(), &PreflightFailure::NotLoggedIn);
    Ok(())
}

#[test]
fn an_api_key_session_fails_on_its_auth_method() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        auth_method: "api_key".to_owned(),
        ..oauth_parts()
    })?;

    assert_eq!(
        failed.into_parts(),
        (
            EventPayload::PreflightFailed(Preflight {
                auth_method: "api_key".to_owned(),
                cli_version: "2.1.296".to_owned(),
                ..unobserved()
            }),
            PreflightFailure::AuthMethod {
                found: "api_key".to_owned()
            }
        )
    );
    Ok(())
}

#[test]
fn a_third_party_provider_fails_on_its_auth_method_first() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        auth_method: "third_party".to_owned(),
        api_provider: "bedrock".to_owned(),
        ..oauth_parts()
    })?;

    assert_eq!(
        failed.reason(),
        &PreflightFailure::AuthMethod {
            found: "third_party".to_owned()
        }
    );
    Ok(())
}

#[test]
fn an_oauth_session_on_another_provider_fails() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        api_provider: "vertex".to_owned(),
        ..oauth_parts()
    })?;

    assert_eq!(
        failed.reason(),
        &PreflightFailure::ApiProvider {
            found: "vertex".to_owned()
        }
    );
    Ok(())
}

#[test]
fn an_unreadable_first_init_fails_init_unclean_with_nothing_observed() -> Result<(), Box<dyn Error>>
{
    let failed = first_call_failure(Err(InitReadError::Missing))?;

    assert_eq!(
        failed.into_parts(),
        (
            EventPayload::PreflightFailed(Preflight {
                auth_method: "oauth_token".to_owned(),
                cli_version: "2.1.296".to_owned(),
                init_clean: false,
                builtins: None,
            }),
            PreflightFailure::InitUnreadable(InitReadError::Missing)
        )
    );
    Ok(())
}

#[test]
fn a_builtin_off_the_pinned_allowlist_fails_init_unclean_with_what_was_seen()
-> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.skills.push("planted".to_owned());
    let mut seen = observed_set();
    seen.skills.push("planted".to_owned());

    let failed = first_call_failure(Ok(SessionInit::new(parts)?))?;

    assert_eq!(
        failed.into_parts(),
        (
            EventPayload::PreflightFailed(Preflight {
                auth_method: "oauth_token".to_owned(),
                cli_version: "2.1.296".to_owned(),
                init_clean: false,
                builtins: Some(seen),
            }),
            PreflightFailure::InitUnclean(InitUnclean::Unlisted {
                kind: BuiltinKind::Skill,
                name: "planted".to_owned(),
            })
        )
    );
    Ok(())
}

#[test]
fn an_mcp_server_in_the_first_init_fails_init_unclean() -> Result<(), Box<dyn Error>> {
    let parts = InitParts {
        mcp_servers: vec!["github".to_owned()],
        ..observed()
    };

    let failed = first_call_failure(Ok(SessionInit::new(parts)?))?;

    assert_eq!(
        failed.reason(),
        &PreflightFailure::InitUnclean(InitUnclean::McpServer {
            name: "github".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_first_init_on_another_cli_reports_that_version() -> Result<(), Box<dyn Error>> {
    let parts = InitParts {
        cli_version: "2.1.297".to_owned(),
        ..observed()
    };

    let failed = first_call_failure(Ok(SessionInit::new(parts)?))?;

    assert_eq!(
        failed.event().cli_version,
        "2.1.297",
        "the event reports the version the session ran"
    );
    Ok(())
}

#[test]
fn every_failure_is_the_llm_unavailable_policy() -> Result<(), Box<dyn Error>> {
    let (version, _) = pinned()?;
    let mut deferrals = vec![
        preflight(["ANTHROPIC_API_KEY"], &probe(oauth_parts())?, &version)
            .err()
            .map(|failed| failed.deferral()),
        proof_with(&FakeProbe(Err(ProbeDown)))?
            .err()
            .map(|failed| failed.deferral()),
    ];
    for parts in [
        StatusParts {
            version: "2.1.297".to_owned(),
            ..oauth_parts()
        },
        StatusParts {
            logged_in: false,
            ..oauth_parts()
        },
        StatusParts {
            auth_method: "api_key".to_owned(),
            ..oauth_parts()
        },
        StatusParts {
            api_provider: "bedrock".to_owned(),
            ..oauth_parts()
        },
    ] {
        deferrals.push(Some(failure_with(parts)?.deferral()));
    }
    deferrals.push(Some(
        first_call_failure(Err(InitReadError::Repeated))?.deferral(),
    ));

    assert_eq!(deferrals, vec![Some(Deferral::InfraLlmUnavailable); 7]);
    Ok(())
}

#[test]
fn a_status_name_of_the_longest_readable_length_is_kept() -> Result<(), Box<dyn Error>> {
    let longest = "m".repeat(MAX_BUILTIN_NAME_BYTES);

    let status = CliStatus::new(StatusParts {
        auth_method: longest.clone(),
        ..oauth_parts()
    })?;

    assert_eq!(status.auth_method(), longest);
    Ok(())
}

#[test]
fn a_status_name_one_byte_too_long_is_refused() {
    let status = CliStatus::new(StatusParts {
        api_provider: "p".repeat(MAX_BUILTIN_NAME_BYTES + 1),
        ..oauth_parts()
    });

    assert_eq!(
        status,
        Err(StatusReadError::Name {
            field: "apiProvider"
        })
    );
}

#[test]
fn an_empty_or_control_character_status_name_is_refused() {
    let empty = CliStatus::new(StatusParts {
        version: String::new(),
        ..oauth_parts()
    });
    let control = CliStatus::new(StatusParts {
        auth_method: "oauth_token\n".to_owned(),
        ..oauth_parts()
    });

    assert_eq!(
        (empty, control),
        (
            Err(StatusReadError::Name { field: "version" }),
            Err(StatusReadError::Name {
                field: "authMethod"
            })
        )
    );
}

fn event(payload: EventPayload) -> Result<Event, Box<dyn Error>> {
    let id = |n| std::num::NonZeroU64::new(n).ok_or("ids are positive");
    Ok(Event {
        schema: EventSchema::V1,
        job: "review".to_owned(),
        idx: 3,
        ts: "2026-10-13T06:20:00Z".to_owned(),
        run_key: "run-83-1".to_owned(),
        run_uid: RunUid {
            repo_id: id(840_213_377)?,
            run_id: id(18_342_009_871)?,
            run_attempt: id(1)?,
        },
        stage: "S3".to_owned(),
        payload,
    })
}

#[test]
fn both_preflight_events_satisfy_the_event_schema() -> Result<(), Box<dyn Error>> {
    let validator = ContractValidator::new()?;
    let (_, allowlist) = pinned()?;
    let passed = proof()?
        .check_first_call::<Infallible>(Ok(SessionInit::new(observed())?), &allowlist)
        .map_err(|failed| format!("{failed:?}"))?
        .into_event();
    let failed = proof_with(&FakeProbe(Err(ProbeDown)))?
        .err()
        .ok_or("the probe failed")?
        .into_parts()
        .0;

    for payload in [passed, failed] {
        let bytes = serde_json::to_vec(&event(payload)?)?;
        validator.read(Contract::Event, &bytes)?;
    }
    Ok(())
}

#[test]
fn the_runbook_the_link_names_covers_setup_token_and_the_environment_secret() {
    let runbook = include_str!("../../../../docs/operations/token-rotation.md");

    assert!(
        TOKEN_ROTATION_RUNBOOK.ends_with("/docs/operations/token-rotation.md"),
        "the link names the file read here: {TOKEN_ROTATION_RUNBOOK}"
    );

    for needle in [
        "claude setup-token",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "assay-review",
        "ASSAY_TOKEN_EXPIRES_ON",
    ] {
        assert!(runbook.contains(needle), "the runbook names {needle}");
    }
}

#[test]
fn a_session_that_is_not_logged_in_links_the_rotation_runbook() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        logged_in: false,
        auth_method: "none".to_owned(),
        ..oauth_parts()
    })?;

    assert_eq!(
        failed.reason().to_string(),
        format!(
            "the CLI is not logged in; rotate the OAuth token: \
             {TOKEN_ROTATION_RUNBOOK}"
        )
    );
    assert_eq!(failed.reason().runbook(), Some(TOKEN_ROTATION_RUNBOOK));
    Ok(())
}

#[test]
fn a_session_on_another_auth_method_links_the_rotation_runbook() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        auth_method: "claude.ai".to_owned(),
        ..oauth_parts()
    })?;

    assert_eq!(
        failed.reason().to_string(),
        format!(
            "the CLI authenticates with \"claude.ai\", not oauth_token; \
             rotate the OAuth token: {TOKEN_ROTATION_RUNBOOK}"
        )
    );
    assert_eq!(failed.reason().runbook(), Some(TOKEN_ROTATION_RUNBOOK));
    Ok(())
}

#[test]
fn failures_that_rotation_does_not_fix_link_no_runbook() -> Result<(), Box<dyn Error>> {
    let (version, _) = pinned()?;
    let variable = preflight(["ANTHROPIC_API_KEY"], &probe(oauth_parts())?, &version)
        .err()
        .ok_or("a provider variable fails")?;
    let provider = failure_with(StatusParts {
        api_provider: "bedrock".to_owned(),
        ..oauth_parts()
    })?;
    let version = failure_with(StatusParts {
        version: "2.1.297".to_owned(),
        ..oauth_parts()
    })?;
    let init = first_call_failure(Err(InitReadError::Repeated))?;

    assert_eq!(
        [
            variable.reason().runbook(),
            provider.reason().runbook(),
            version.reason().runbook(),
            init.reason().runbook(),
        ],
        [None; 4]
    );
    assert!(
        !variable
            .reason()
            .to_string()
            .contains(TOKEN_ROTATION_RUNBOOK),
        "{variable}"
    );
    Ok(())
}

#[test]
fn a_failed_preflight_leaves_its_reason_to_its_source() -> Result<(), Box<dyn Error>> {
    let failed = failure_with(StatusParts {
        logged_in: false,
        ..oauth_parts()
    })?;

    let shown = failed.to_string();
    let source = failed.source().map(ToString::to_string);

    assert_eq!(
        (shown.as_str(), source),
        ("preflight failed", Some(failed.reason().to_string()))
    );
    Ok(())
}

#[test]
fn a_failed_preflight_names_its_reason_as_its_source() -> Result<(), Box<dyn Error>> {
    let failed = proof_with(&FakeProbe(Err(ProbeDown)))?
        .err()
        .ok_or("the probe failed")?;

    let source = failed.source().map(ToString::to_string);
    let cause = failed
        .source()
        .and_then(Error::source)
        .map(ToString::to_string);

    assert_eq!(
        (source.as_deref(), cause.as_deref()),
        (
            Some("the CLI's version and auth status could not be read"),
            Some("the CLI could not be started")
        )
    );
    Ok(())
}

#[test]
fn an_unclean_first_init_is_the_llm_unavailable_policy() -> Result<(), Box<dyn Error>> {
    let parts = InitParts {
        mcp_servers: vec!["github".to_owned()],
        ..observed()
    };

    let failed = first_call_failure(Ok(SessionInit::new(parts)?))?;

    assert_eq!(failed.deferral(), Deferral::InfraLlmUnavailable);
    Ok(())
}
