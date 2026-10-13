//! `doctor`'s session checks (03 §8, #121): one canary call in a throwaway checkout with planted
//! mods and AGENTS.md/CLAUDE.md instructions, judged for a clean init, allowlist drift, a loaded
//! or named mod and a followed instruction.

use std::cell::RefCell;
use std::error::Error;
use std::fmt;

use assay_app::{
    AllowlistDrift, ArtifactNonce, BuiltinKind, CanaryProbe, CanaryRun, CanaryToken, Check,
    CheckFailure, DoctorError, InitParts, InitReadError, InitUnclean, ModelOutcome, NonceSource,
    ParseNonceError, SessionInit, Structured, doctor,
};
use assay_domain::BuiltinSet;
use serde_json::json;

use crate::session_init::{builtin, observed, pinned};

const NONCE: &str = "0123456789abcdef0123456789abcdef";
const WORD: &str = "ASSAYCANARY0123456789abcdef0123456789abcdef";

#[derive(Debug)]
struct Failed(&'static str);

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Error for Failed {}

/// A nonce source that gives its one nonce, or fails when it has none.
struct FixedNonce(Option<&'static str>);

impl NonceSource for FixedNonce {
    type Error = ParseNonceError;

    fn nonce(&mut self) -> Result<ArtifactNonce, ParseNonceError> {
        self.0.ok_or(ParseNonceError)?.parse()
    }
}

/// Each check with its failure, in report order.
type Results = Vec<(Check, Option<CheckFailure>)>;

/// A probe that answers with one scripted run and records the token it was given.
struct ScriptedProbe {
    run: RefCell<Option<CanaryRun>>,
    // Interior mutability: `CanaryProbe::probe` takes `&self`.
    tokens: RefCell<Vec<String>>,
}

impl ScriptedProbe {
    fn new(run: CanaryRun) -> Self {
        Self {
            run: RefCell::new(Some(run)),
            tokens: RefCell::default(),
        }
    }
}

impl CanaryProbe for ScriptedProbe {
    type Error = Failed;

    fn probe(&self, token: &CanaryToken) -> Result<CanaryRun, Failed> {
        self.tokens.borrow_mut().push(token.suffix().to_owned());
        self.run.borrow_mut().take().ok_or(Failed("probed twice"))
    }
}

struct FailingProbe;

impl CanaryProbe for FailingProbe {
    type Error = Failed;

    fn probe(&self, _: &CanaryToken) -> Result<CanaryRun, Failed> {
        Err(Failed("the CLI could not start"))
    }
}

fn answered(reply: &str) -> ModelOutcome {
    ModelOutcome::Ok(Structured::new(json!({ "reply": reply })))
}

/// A run in which nothing leaked: a clean init, an answer without the word, no marker.
fn clean_run() -> Result<CanaryRun, Box<dyn Error>> {
    Ok(CanaryRun {
        init: Ok(SessionInit::new(observed())?),
        outcome: answered("ready"),
        transcript: br#"{"type":"result","structured_output":{"reply":"ready"}}"#.to_vec(),
        planted_mods: vec![
            "assay-canary-user".to_owned(),
            "assay-canary-skill".to_owned(),
        ],
        loaded_mods: Vec::new(),
    })
}

/// The checks' failures, in report order.
fn failures(run: CanaryRun) -> Result<Results, Box<dyn Error>> {
    let (version, allowlist) = pinned()?;
    let probe = ScriptedProbe::new(run);
    let report = doctor(&probe, &mut FixedNonce(Some(NONCE)), &version, &allowlist)?;
    Ok(report
        .results()
        .iter()
        .map(|result| (result.check, result.failure.clone()))
        .collect())
}

fn all_passing() -> Results {
    vec![
        (Check::CleanInit, None),
        (Check::BuiltinAllowlist, None),
        (Check::ModCanary, None),
        (Check::AgentsMdCanary, None),
    ]
}

fn passing_except(check: Check, failure: CheckFailure) -> Results {
    let mut results = all_passing();
    if let Some(result) = results.iter_mut().find(|(each, _)| *each == check) {
        result.1 = Some(failure);
    }
    results
}

#[test]
fn a_clean_canary_run_passes_every_check() -> Result<(), Box<dyn Error>> {
    let (version, allowlist) = pinned()?;
    let probe = ScriptedProbe::new(clean_run()?);

    let report = doctor(&probe, &mut FixedNonce(Some(NONCE)), &version, &allowlist)?;

    assert!(report.passed());
    let results: Results = report
        .results()
        .iter()
        .map(|result| (result.check, result.failure.clone()))
        .collect();
    assert_eq!(results, all_passing());
    Ok(())
}

#[test]
fn the_probe_gets_the_word_in_two_parts() -> Result<(), Box<dyn Error>> {
    let (version, allowlist) = pinned()?;
    let probe = ScriptedProbe::new(clean_run()?);

    doctor(&probe, &mut FixedNonce(Some(NONCE)), &version, &allowlist)?;

    assert_eq!(CanaryToken::PREFIX, "ASSAYCANARY");
    assert_eq!(probe.tokens.take(), [NONCE.to_owned()]);
    Ok(())
}

#[test]
fn an_unreadable_init_fails_every_check_that_reads_it() -> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        init: Err(InitReadError::Missing),
        ..clean_run()?
    };

    let failures = failures(run)?;

    let unreadable = Some(CheckFailure::InitUnreadable(InitReadError::Missing));
    assert_eq!(
        failures,
        [
            (Check::CleanInit, unreadable.clone()),
            (Check::BuiltinAllowlist, unreadable.clone()),
            (Check::ModCanary, unreadable),
            (Check::AgentsMdCanary, None),
        ]
    );
    Ok(())
}

#[test]
fn an_unclean_init_fails_the_clean_init_check() -> Result<(), Box<dyn Error>> {
    let parts = InitParts {
        mcp_servers: vec!["github".to_owned()],
        ..observed()
    };
    let run = CanaryRun {
        init: Ok(SessionInit::new(parts)?),
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(
            Check::CleanInit,
            CheckFailure::InitUnclean(InitUnclean::McpServer {
                name: "github".to_owned()
            })
        )
    );
    Ok(())
}

#[test]
fn a_built_in_missing_from_the_session_is_drift() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.agents.retain(|agent| agent != "Plan");
    let run = CanaryRun {
        init: Ok(SessionInit::new(parts)?),
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(
            Check::BuiltinAllowlist,
            CheckFailure::Drift(AllowlistDrift {
                unlisted: BuiltinSet::default(),
                unobserved: BuiltinSet {
                    agents: vec!["Plan".to_owned()],
                    ..BuiltinSet::default()
                },
            })
        )
    );
    Ok(())
}

#[test]
fn a_mod_whose_marker_was_written_fails_the_mod_canary() -> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        loaded_mods: vec!["assay-canary-skill".to_owned()],
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(
            Check::ModCanary,
            CheckFailure::ModLoaded {
                name: "assay-canary-skill".to_owned()
            }
        )
    );
    Ok(())
}

#[test]
fn a_mod_named_in_init_plugins_fails_the_mod_canary() -> Result<(), Box<dyn Error>> {
    let mut parts = observed();
    parts.plugins.push(builtin("assay-canary-user"));
    let run = CanaryRun {
        init: Ok(SessionInit::new(parts)?),
        ..clean_run()?
    };

    let failures = failures(run)?;

    assert_eq!(
        failures.get(2),
        Some(&(
            Check::ModCanary,
            Some(CheckFailure::ModNamed {
                name: "assay-canary-user".to_owned()
            })
        ))
    );
    Ok(())
}

#[test]
fn a_canary_that_planted_no_mod_fails() -> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        planted_mods: Vec::new(),
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(Check::ModCanary, CheckFailure::NothingPlanted)
    );
    Ok(())
}

#[test]
fn the_word_in_the_output_fails_the_agents_md_canary() -> Result<(), Box<dyn Error>> {
    let reply = format!("ready {WORD}");
    let run = CanaryRun {
        outcome: answered(&reply),
        transcript: format!(r#"{{"structured_output":{{"reply":"{reply}"}}}}"#).into_bytes(),
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(Check::AgentsMdCanary, CheckFailure::InstructionFollowed)
    );
    Ok(())
}

#[test]
fn the_word_in_another_case_still_fails_the_agents_md_canary() -> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        transcript: WORD.to_ascii_lowercase().into_bytes(),
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(Check::AgentsMdCanary, CheckFailure::InstructionFollowed)
    );
    Ok(())
}

#[test]
fn half_the_word_alone_passes_the_agents_md_canary() -> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        transcript: format!("`ASSAYCANARY` immediately followed by `{NONCE}`").into_bytes(),
        ..clean_run()?
    };

    assert_eq!(failures(run)?, all_passing());
    Ok(())
}

#[test]
fn the_nonce_in_the_models_own_text_fails_the_agents_md_canary() -> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        transcript: format!(
            "{}\n{}\n",
            r#"{"type":"system","subtype":"init"}"#,
            json!({
                "type": "assistant",
                "message": {"content": [{"type": "text", "text": format!("ready {NONCE}")}]}
            })
        )
        .into_bytes(),
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(Check::AgentsMdCanary, CheckFailure::NonceWritten)
    );
    Ok(())
}

#[test]
fn the_nonce_in_the_result_in_another_case_fails_the_agents_md_canary() -> Result<(), Box<dyn Error>>
{
    let run = CanaryRun {
        transcript: json!({
            "type": "result",
            "result": format!("ready ASSAYCANARY {}", NONCE.to_ascii_uppercase())
        })
        .to_string()
        .into_bytes(),
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(Check::AgentsMdCanary, CheckFailure::NonceWritten)
    );
    Ok(())
}

#[test]
fn the_nonce_in_a_tool_result_alone_passes_the_agents_md_canary() -> Result<(), Box<dyn Error>> {
    let tool_result = json!({
        "type": "user",
        "message": {"content": [{
            "type": "tool_result",
            "content": format!("`ASSAYCANARY` immediately followed by `{NONCE}`")
        }]}
    });
    let run = CanaryRun {
        transcript: format!(
            "{tool_result}\n{}\n",
            r#"{"type":"result","result":"ready"}"#
        )
        .into_bytes(),
        ..clean_run()?
    };

    assert_eq!(failures(run)?, all_passing());
    Ok(())
}

#[test]
fn a_mod_that_ran_fails_the_mod_canary_even_when_the_init_is_unreadable()
-> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        init: Err(InitReadError::Missing),
        loaded_mods: vec!["assay-canary-user".to_owned()],
        ..clean_run()?
    };

    let failures = failures(run)?;

    assert_eq!(
        failures.get(2),
        Some(&(
            Check::ModCanary,
            Some(CheckFailure::ModLoaded {
                name: "assay-canary-user".to_owned()
            })
        ))
    );
    Ok(())
}

#[test]
fn a_call_that_did_not_answer_fails_the_agents_md_canary() -> Result<(), Box<dyn Error>> {
    let run = CanaryRun {
        outcome: ModelOutcome::QuotaExhausted,
        ..clean_run()?
    };

    assert_eq!(
        failures(run)?,
        passing_except(Check::AgentsMdCanary, CheckFailure::NoAnswer)
    );
    Ok(())
}

#[test]
fn a_probe_that_fails_fails_the_doctor() -> Result<(), Box<dyn Error>> {
    let (version, allowlist) = pinned()?;

    let result = doctor(
        &FailingProbe,
        &mut FixedNonce(Some(NONCE)),
        &version,
        &allowlist,
    );

    assert!(
        matches!(
            result,
            Err(DoctorError::Probe(Failed("the CLI could not start")))
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_nonce_that_cannot_be_drawn_fails_the_doctor_before_probing() -> Result<(), Box<dyn Error>> {
    let (version, allowlist) = pinned()?;
    let probe = ScriptedProbe::new(clean_run()?);

    let result = doctor(&probe, &mut FixedNonce(None), &version, &allowlist);

    assert!(
        matches!(result, Err(DoctorError::Nonce(ParseNonceError))),
        "{result:?}"
    );
    assert!(probe.tokens.take().is_empty());
    Ok(())
}

#[test]
fn each_check_and_failure_reads_as_one_report_line() {
    let lines: Vec<String> = Check::ALL.iter().map(ToString::to_string).collect();

    assert_eq!(
        lines,
        [
            "clean-init",
            "builtin-allowlist",
            "mod-canary",
            "agents-md-canary"
        ]
    );
    assert_eq!(
        CheckFailure::ModLoaded {
            name: "assay-canary-skill".to_owned()
        }
        .to_string(),
        "mod \"assay-canary-skill\" ran its session.start hook"
    );
    assert_eq!(
        CheckFailure::Drift(AllowlistDrift {
            unlisted: BuiltinSet {
                skills: vec!["new".to_owned()],
                ..BuiltinSet::default()
            },
            unobserved: BuiltinSet::default(),
        })
        .to_string(),
        "the observed built-ins differ from claude_cli_builtins: + skill \"new\""
    );
    assert_eq!(
        CheckFailure::InitUnclean(InitUnclean::Unlisted {
            kind: BuiltinKind::Agent,
            name: "x".to_owned()
        })
        .to_string(),
        "init-unclean: agent \"x\" is not on the built-in allowlist"
    );
}
