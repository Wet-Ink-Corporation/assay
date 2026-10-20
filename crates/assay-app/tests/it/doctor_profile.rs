//! `verify_profile` (03 §8, A-10, #84): the pin's minimum, the preflight, and one canary call
//! with the production argv plus `--include-hook-events` in a sandbox planted with hooks, judged
//! for the flags it shows, the hook canary, the fence canary and the `--add-dir` boundary.

use std::error::Error;

use assay_app::{
    FENCE_TARGET, HeadlessFlag, InitParts, InitReadError, InitUnclean, MIN_CLI_VERSION,
    ModelOutcome, PROFILE_SCHEMA, PermissionDenial, PreflightFailure, ProfileCheck, ProfileError,
    ProfileFailure, ProfilePorts, ProfileRun, ProfileWords, ReplyField, SessionInit,
    verify_profile,
};
use serde_json::Value;

use crate::doctor_profile_fakes::{
    Failed, FailingProfile, Failures, FixedNonce, NONCE, OUTSIDE_FILE, OUTSIDE_WORD, PROMPT_WORD,
    Pins, SYSTEM_WORD, ScriptedProfile, TOKEN, TREE_FILE, TREE_WORD, clean_env, clean_run, cli,
    failures_of, read, reply, report_for, version,
};
use crate::session_init::observed;

fn all_passing() -> Failures {
    ProfileCheck::ALL.map(|check| (check, None)).to_vec()
}

fn passing_except(failed: &[(ProfileCheck, ProfileFailure<Failed>)]) -> Failures {
    let mut results = all_passing();
    for (check, failure) in failed {
        if let Some(result) = results.iter_mut().find(|(each, _)| each == check) {
            result.1 = Some(failure.clone());
        }
    }
    results
}

fn failures(build: impl Fn(&ProfileWords) -> ProfileRun) -> Result<Failures, Box<dyn Error>> {
    Ok(failures_of(&report_for(build)?))
}

#[test]
fn a_run_that_shows_every_flag_honoured_passes_every_check() -> Result<(), Box<dyn Error>> {
    let report = report_for(clean_run)?;

    assert!(report.passed());
    assert_eq!(failures_of(&report), all_passing());
    Ok(())
}

#[test]
fn the_checks_are_reported_in_a_fixed_order_under_stable_names() {
    let names: Vec<String> = ProfileCheck::ALL.iter().map(ToString::to_string).collect();

    assert_eq!(
        names,
        [
            "cli-minimum",
            "preflight",
            "profile-accepted",
            "permission-mode",
            "stdin-prompt",
            "system-prompt-file",
            "hook-canary",
            "fence-canary",
            "add-dir",
            "outside-read",
        ]
    );
}

#[test]
fn the_probe_gets_four_words_from_four_nonces() -> Result<(), Box<dyn Error>> {
    let pins = Pins::new()?;
    let probe = ScriptedProfile::new(clean_run);
    let mut nonces = FixedNonce::default();

    let ports = ProfilePorts {
        cli: &cli("2.1.296")?,
        probe: &probe,
        nonces: &mut nonces,
    };

    verify_profile(clean_env(), ports, &pins.context())?;

    assert_eq!(nonces.drawn, 4);
    assert_eq!(
        probe.words.take(),
        [PROMPT_WORD, SYSTEM_WORD, TREE_WORD, OUTSIDE_WORD]
    );
    Ok(())
}

#[test]
fn words_are_a_fixed_prefix_and_the_nonce() -> Result<(), Box<dyn Error>> {
    let nonce = NONCE.parse()?;

    let words = ProfileWords::new([&nonce, &nonce, &nonce, &nonce]);

    assert_eq!(
        [
            words.prompt(),
            words.system(),
            words.tree(),
            words.outside()
        ],
        [PROMPT_WORD, SYSTEM_WORD, TREE_WORD, OUTSIDE_WORD]
    );
    Ok(())
}

#[test]
fn the_schema_asks_for_exactly_the_three_reply_words() -> Result<(), Box<dyn Error>> {
    let schema: Value = serde_json::from_str(PROFILE_SCHEMA)?;

    assert_eq!(
        schema["required"],
        serde_json::json!(["prompt_word", "system_word", "tree_word"])
    );
    assert_eq!(schema["additionalProperties"], Value::Bool(false));
    assert_eq!(
        [ReplyField::Prompt, ReplyField::System, ReplyField::Tree].map(ReplyField::key),
        ["prompt_word", "system_word", "tree_word"]
    );
    Ok(())
}

#[test]
fn the_minimum_and_the_fence_target_are_those_of_03_section_8() {
    assert_eq!(MIN_CLI_VERSION, "2.1.257");
    assert_eq!(FENCE_TARGET, "//proc/self/environ");
}

fn minimum_failure(pin: &str) -> Result<Option<ProfileFailure<Failed>>, Box<dyn Error>> {
    let pins = Pins::new()?;
    let pinned = version(pin)?;
    let context = assay_app::ProfileContext {
        pinned: &pinned,
        ..pins.context()
    };
    let ports = ProfilePorts {
        cli: &cli(pin)?,
        probe: &ScriptedProfile::new(clean_run),
        nonces: &mut FixedNonce::default(),
    };
    let report = verify_profile(clean_env(), ports, &context)?;
    let minimum = report
        .results()
        .iter()
        .find(|result| result.check == ProfileCheck::CliMinimum)
        .ok_or("no cli-minimum result")?;
    Ok(minimum.failure.clone())
}

#[test]
fn a_pin_at_the_minimum_passes_and_one_below_fails() -> Result<(), Box<dyn Error>> {
    assert_eq!(minimum_failure("2.1.257")?, None);
    assert_eq!(minimum_failure("2.2.0")?, None);
    assert_eq!(minimum_failure("10.0.0")?, None);
    assert_eq!(
        minimum_failure("2.1.256")?,
        Some(ProfileFailure::BelowMinimum {
            pinned: "2.1.256".to_owned()
        })
    );
    assert_eq!(
        minimum_failure("1.9.999")?,
        Some(ProfileFailure::BelowMinimum {
            pinned: "1.9.999".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_pin_too_large_to_compare_fails_closed() -> Result<(), Box<dyn Error>> {
    let pin = "2.1.99999999999999999999";

    assert_eq!(
        minimum_failure(pin)?,
        Some(ProfileFailure::VersionUnreadable {
            pinned: pin.to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_failed_preflight_makes_no_call_and_runs_no_other_check() -> Result<(), Box<dyn Error>> {
    let pins = Pins::new()?;
    let probe = ScriptedProfile::new(clean_run);
    let mut nonces = FixedNonce::default();

    let ports = ProfilePorts {
        cli: &cli("2.1.296")?,
        probe: &probe,
        nonces: &mut nonces,
    };

    let report = verify_profile(["PATH", "ANTHROPIC_API_KEY"], ports, &pins.context())?;

    let mut expected: Failures = ProfileCheck::ALL
        .map(|check| (check, Some(ProfileFailure::NotRun)))
        .to_vec();
    if let [minimum, preflight, ..] = expected.as_mut_slice() {
        minimum.1 = None;
        preflight.1 = Some(ProfileFailure::Preflight(
            PreflightFailure::ProviderVariable {
                name: "ANTHROPIC_API_KEY".to_owned(),
            },
        ));
    }
    assert_eq!(failures_of(&report), expected);
    assert_eq!((probe.calls.get(), nonces.drawn), (0, 0));
    Ok(())
}

#[test]
fn a_cli_that_is_not_the_pin_fails_the_preflight() -> Result<(), Box<dyn Error>> {
    let pins = Pins::new()?;

    let ports = ProfilePorts {
        cli: &cli("2.1.295")?,
        probe: &ScriptedProfile::new(clean_run),
        nonces: &mut FixedNonce::default(),
    };

    let report = verify_profile(clean_env(), ports, &pins.context())?;

    assert_eq!(
        report.results().get(1).map(|result| result.failure.clone()),
        Some(Some(ProfileFailure::Preflight(
            PreflightFailure::CliVersion {
                found: "2.1.295".to_owned(),
                pinned: "2.1.296".to_owned(),
            }
        )))
    );
    Ok(())
}

#[test]
fn an_unclean_init_of_the_call_fails_the_preflight() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        init: SessionInit::new(InitParts {
            tools: vec!["Bash".to_owned(), "Glob".to_owned(), "Grep".to_owned()],
            ..observed()
        }),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::Preflight,
            ProfileFailure::Preflight(PreflightFailure::InitUnclean(InitUnclean::Tools {
                found: vec!["Bash".to_owned(), "Glob".to_owned(), "Grep".to_owned()],
            }))
        )])
    );
    Ok(())
}

#[test]
fn an_unreadable_init_fails_the_preflight_and_the_profile() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        init: Err(InitReadError::Missing),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[
            (
                ProfileCheck::Preflight,
                ProfileFailure::Preflight(PreflightFailure::InitUnreadable(InitReadError::Missing))
            ),
            (
                ProfileCheck::ProfileAccepted,
                ProfileFailure::InitUnreadable(InitReadError::Missing)
            ),
        ])
    );
    Ok(())
}

#[test]
fn a_call_that_did_not_answer_fails_every_check_that_needs_the_answer() -> Result<(), Box<dyn Error>>
{
    let failures = failures(|words| ProfileRun {
        outcome: ModelOutcome::Malformed,
        ..clean_run(words)
    })?;

    let no_answer = ProfileFailure::NoAnswer {
        outcome: "malformed",
    };
    assert_eq!(
        failures,
        passing_except(&[
            (
                ProfileCheck::ProfileAccepted,
                ProfileFailure::ProfileRefused {
                    outcome: "malformed"
                }
            ),
            (ProfileCheck::StdinPrompt, no_answer.clone()),
            (ProfileCheck::SystemPromptFile, no_answer.clone()),
            (ProfileCheck::HookCanary, no_answer.clone()),
            (ProfileCheck::AddDir, no_answer),
        ])
    );
    Ok(())
}

#[test]
fn a_call_with_neither_init_nor_answer_is_a_refused_profile() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        init: Err(InitReadError::Missing),
        outcome: ModelOutcome::Malformed,
        ..clean_run(words)
    })?;

    let accepted = failures
        .iter()
        .find(|(check, _)| *check == ProfileCheck::ProfileAccepted)
        .map(|(_, failure)| failure.clone());
    assert_eq!(
        accepted,
        Some(Some(ProfileFailure::ProfileRefused {
            outcome: "malformed"
        }))
    );
    Ok(())
}

#[test]
fn a_permission_mode_other_than_dont_ask_fails() -> Result<(), Box<dyn Error>> {
    let default = failures(|words| ProfileRun {
        permission_mode: Some("default".to_owned()),
        ..clean_run(words)
    })?;
    let missing = failures(|words| ProfileRun {
        permission_mode: None,
        ..clean_run(words)
    })?;

    assert_eq!(
        default,
        passing_except(&[(
            ProfileCheck::PermissionMode,
            ProfileFailure::PermissionMode {
                found: Some("default".to_owned())
            }
        )])
    );
    assert_eq!(
        missing,
        passing_except(&[(
            ProfileCheck::PermissionMode,
            ProfileFailure::PermissionMode { found: None }
        )])
    );
    Ok(())
}

#[test]
fn a_reply_without_the_prompt_word_fails_the_stdin_check() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        outcome: reply("ready", words.system(), words.tree()),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::StdinPrompt,
            ProfileFailure::WordMissing {
                field: ReplyField::Prompt
            }
        )])
    );
    Ok(())
}

#[test]
fn a_reply_without_the_system_word_fails_the_system_prompt_check() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        outcome: reply(words.prompt(), "", words.tree()),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::SystemPromptFile,
            ProfileFailure::WordMissing {
                field: ReplyField::System
            }
        )])
    );
    Ok(())
}

#[test]
fn reply_words_are_compared_trimmed_and_in_any_ascii_case() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        outcome: reply(
            &format!(" {} ", words.prompt().to_ascii_lowercase()),
            words.system(),
            words.tree(),
        ),
        ..clean_run(words)
    })?;

    assert_eq!(failures, all_passing());
    Ok(())
}

#[test]
fn a_hook_that_ran_fails_the_hook_canary() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        fired_hooks: vec!["project".to_owned()],
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::HookCanary,
            ProfileFailure::HookRan {
                scope: "project".to_owned()
            }
        )])
    );
    Ok(())
}

#[test]
fn a_hook_event_in_the_stream_fails_the_hook_canary() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        hook_events: vec!["SessionStart".to_owned()],
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::HookCanary,
            ProfileFailure::HookEvent {
                event: "SessionStart".to_owned()
            }
        )])
    );
    Ok(())
}

#[test]
fn a_hook_canary_with_nothing_planted_fails() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        planted_hooks: Vec::new(),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(ProfileCheck::HookCanary, ProfileFailure::NothingPlanted)])
    );
    Ok(())
}

#[test]
fn an_environ_read_that_was_not_denied_fails_the_fence() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        denials: Some(vec![read(OUTSIDE_FILE)]),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::FenceCanary,
            ProfileFailure::NotDenied {
                path: FENCE_TARGET.to_owned()
            }
        )])
    );
    Ok(())
}

#[test]
fn a_denial_of_another_tool_or_path_does_not_count() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        denials: Some(vec![
            PermissionDenial {
                tool_name: "Grep".to_owned(),
                file_path: Some("/proc/self/environ".to_owned()),
            },
            read("/proc/self/environment"),
            read("proc/self/environ"),
            PermissionDenial {
                tool_name: "Read".to_owned(),
                file_path: None,
            },
            read(OUTSIDE_FILE),
        ]),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::FenceCanary,
            ProfileFailure::NotDenied {
                path: FENCE_TARGET.to_owned()
            }
        )])
    );
    Ok(())
}

#[test]
fn the_fence_target_is_matched_with_one_or_two_leading_slashes() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        denials: Some(vec![read("//proc/self/environ"), read(OUTSIDE_FILE)]),
        ..clean_run(words)
    })?;

    assert_eq!(failures, all_passing());
    Ok(())
}

#[test]
fn unreadable_denials_fail_the_fence_and_the_outside_read() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        denials: None,
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[
            (ProfileCheck::FenceCanary, ProfileFailure::DenialsUnreadable),
            (ProfileCheck::OutsideRead, ProfileFailure::DenialsUnreadable),
        ])
    );
    Ok(())
}

#[test]
fn the_token_in_the_transcript_fails_the_fence() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        transcript: format!(
            "{{\"type\":\"user\",\"tool_use_result\":\"CLAUDE_CODE_OAUTH_TOKEN={TOKEN}\"}}\n"
        )
        .into_bytes(),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(ProfileCheck::FenceCanary, ProfileFailure::TokenInTranscript)])
    );
    Ok(())
}

#[test]
fn the_token_in_base64_fails_the_fence() -> Result<(), Box<dyn Error>> {
    // `printf 'TOKEN=<the token>' | base64`: environ read back and encoded.
    let encoded = "VE9LRU49c2stYW50LW9hdDAxLVEyRnVZWEo1Vkc5clpXNHRabTl5TFhSbGMzUno=";
    let failures = failures(|words| ProfileRun {
        transcript: format!("{{\"type\":\"assistant\",\"text\":\"{encoded}\"}}\n").into_bytes(),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(ProfileCheck::FenceCanary, ProfileFailure::TokenInTranscript)])
    );
    Ok(())
}

#[test]
fn a_transcript_the_scan_cannot_read_back_fails_the_fence() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        transcript: br#"{"type":"assistant","text":"\ud800"}"#.to_vec(),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::FenceCanary,
            ProfileFailure::TranscriptUnscannable
        )])
    );
    Ok(())
}

#[test]
fn a_denied_tree_read_fails_the_add_dir_check() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        denials: Some(vec![
            read("/proc/self/environ"),
            read(OUTSIDE_FILE),
            read(TREE_FILE),
        ]),
        outcome: reply(words.prompt(), words.system(), ""),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::AddDir,
            ProfileFailure::TreeDenied {
                path: TREE_FILE.to_owned()
            }
        )])
    );
    Ok(())
}

#[test]
fn a_reply_without_the_tree_word_fails_the_add_dir_check() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        outcome: reply(words.prompt(), words.system(), "unreadable"),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::AddDir,
            ProfileFailure::WordMissing {
                field: ReplyField::Tree
            }
        )])
    );
    Ok(())
}

#[test]
fn the_outside_word_anywhere_in_the_transcript_fails_the_outside_read() -> Result<(), Box<dyn Error>>
{
    let failures = failures(|words| ProfileRun {
        transcript: format!(
            "{{\"type\":\"user\",\"content\":\"{}\"}}\n",
            words.outside().to_ascii_lowercase()
        )
        .into_bytes(),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(ProfileCheck::OutsideRead, ProfileFailure::OutsideRead)])
    );
    Ok(())
}

#[test]
fn an_outside_read_that_was_not_denied_fails() -> Result<(), Box<dyn Error>> {
    let failures = failures(|words| ProfileRun {
        denials: Some(vec![read("/proc/self/environ")]),
        ..clean_run(words)
    })?;

    assert_eq!(
        failures,
        passing_except(&[(
            ProfileCheck::OutsideRead,
            ProfileFailure::NotDenied {
                path: OUTSIDE_FILE.to_owned()
            }
        )])
    );
    Ok(())
}

#[test]
fn a_probe_that_cannot_run_is_an_error() -> Result<(), Box<dyn Error>> {
    let pins = Pins::new()?;

    let ports = ProfilePorts {
        cli: &cli("2.1.296")?,
        probe: &FailingProfile,
        nonces: &mut FixedNonce::default(),
    };

    let verified = verify_profile(clean_env(), ports, &pins.context());

    assert!(
        matches!(
            verified,
            Err(ProfileError::Probe(Failed("the CLI could not start")))
        ),
        "{verified:?}"
    );
    Ok(())
}

/// Each flag's line of the report for a run built by `build`: whether it passed, and its text.
fn flag_lines(
    build: impl Fn(&ProfileWords) -> ProfileRun,
) -> Result<Vec<(bool, String)>, Box<dyn Error>> {
    Ok(report_for(build)?
        .flags()
        .map(|result| (result.failed.is_none(), result.to_string()))
        .collect())
}

#[test]
fn every_flag_of_the_profile_is_reported_with_its_evidence() -> Result<(), Box<dyn Error>> {
    let lines = flag_lines(clean_run)?;

    assert_eq!(
        lines,
        [
            "-p (prompt on stdin): shown by stdin-prompt",
            "--output-format stream-json: shown by profile-accepted",
            "--verbose: shown by profile-accepted",
            "--json-schema: shown by stdin-prompt, system-prompt-file, add-dir",
            "--append-system-prompt-file: shown by system-prompt-file",
            "--exclude-dynamic-system-prompt-sections: accepted (profile-accepted), its effect \
             is not exercised",
            "--restricted: shown by profile-accepted, preflight, outside-read",
            "--safe-mode: shown by profile-accepted, hook-canary",
            "--settings: shown by fence-canary, outside-read, hook-canary",
            "--tools Read,Grep,Glob: shown by preflight",
            "--add-dir: shown by add-dir",
            "--permission-mode dontAsk: shown by permission-mode",
            "--max-turns: accepted (profile-accepted), its effect is not exercised",
            "--max-budget-usd: accepted (profile-accepted), its effect is not exercised",
            "--include-hook-events: accepted (hook-canary), its effect is not exercised",
        ]
        .map(|line| (true, line.to_owned()))
    );
    assert_eq!(HeadlessFlag::ALL.len(), lines.len());
    Ok(())
}

#[test]
fn a_failed_check_fails_every_flag_it_shows() -> Result<(), Box<dyn Error>> {
    let lines = flag_lines(|words| ProfileRun {
        denials: Some(vec![read("/proc/self/environ")]),
        ..clean_run(words)
    })?;

    let failed: Vec<&str> = lines
        .iter()
        .filter(|(ok, _)| !ok)
        .map(|(_, line)| line.as_str())
        .collect();
    assert_eq!(
        failed,
        [
            "--restricted: outside-read failed",
            "--settings: outside-read failed"
        ]
    );
    Ok(())
}

#[test]
fn a_failed_preflight_fails_every_flag() -> Result<(), Box<dyn Error>> {
    let pins = Pins::new()?;

    let ports = ProfilePorts {
        cli: &cli("2.1.296")?,
        probe: &ScriptedProfile::new(clean_run),
        nonces: &mut FixedNonce::default(),
    };

    let report = verify_profile(["ANTHROPIC_API_KEY"], ports, &pins.context())?;

    assert!(report.flags().all(|result| result.failed.is_some()));
    assert_eq!(report.flags().count(), HeadlessFlag::ALL.len());
    Ok(())
}

#[test]
fn the_report_gives_the_flag_results_of_its_checks() -> Result<(), Box<dyn Error>> {
    let report = report_for(|words| ProfileRun {
        permission_mode: None,
        ..clean_run(words)
    })?;

    let failed: Vec<String> = report
        .flags()
        .filter(|result| result.failed.is_some())
        .map(|result| result.to_string())
        .collect();
    assert_eq!(
        failed,
        ["--permission-mode dontAsk: permission-mode failed"]
    );
    Ok(())
}
