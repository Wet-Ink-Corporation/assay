//! `ClaudeHeadless` against a stand-in `claude` (`tests/fixtures/fake-claude`): the 03 §8
//! invocation, the shared empty working directory, the cleared environment and the bounds.

use std::assert_matches;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{ModelOutcome, ModelRunner};
use assay_domain::Aeu;
use assay_runner::{
    ClaudeHeadless, EnvVarError, HeadlessEnv, HeadlessError, Workdir, WorkdirError,
};
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{BUDGET, call, fixtures_dir, stream_path};

const TOKEN: &str = "sk-ant-oat01-test-token";

/// A scratch directory with an empty working directory and a directory for the fake's records.
struct Scene {
    root: TempDir,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("empty"))?;
        fs::create_dir(root.path().join("record"))?;
        Ok(Self { root })
    }

    fn workdir(&self) -> PathBuf {
        self.root.path().join("empty")
    }

    fn record(&self, name: &str) -> PathBuf {
        self.root.path().join("record").join(name)
    }

    /// A runner whose fake `claude` prints the stream `name`, then hangs when `hang`.
    fn runner(&self, name: &str, hang: bool) -> Result<ClaudeHeadless, Box<dyn Error>> {
        let mut env = HeadlessEnv::new(TOKEN);
        env.pass("FAKE_RECORD", self.root.path().join("record"))?;
        env.pass("FAKE_STREAM", stream_path(name))?;
        if hang {
            env.pass("FAKE_HANG", "1")?;
        }
        Ok(ClaudeHeadless::new(
            fixtures_dir().join("fake-claude"),
            Workdir::new(self.workdir())?,
            env,
        ))
    }

    /// The arguments the fake was started with.
    fn argv(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let raw = String::from_utf8(fs::read(self.record("argv"))?)?;
        Ok(raw.split_terminator('\0').map(str::to_owned).collect())
    }

    /// The environment the fake saw.
    fn env(&self) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
        let raw = fs::read_to_string(self.record("env"))?;
        Ok(raw
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect())
    }
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn the_invocation_is_exactly_the_headless_profile() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    scene.runner("ok", false)?.run(&call()?)?;

    // 03 §8, flag for flag; the settings JSON is the spec's, byte for byte. The prompt is not an
    // argument: it arrives on stdin (wi-9fe37e).
    let settings = r#"{"disableAllHooks":true,"permissions":{"deny":["Read(//proc/**)","Read(//sys/**)","Read(~/**)","Read(//etc/**)"],"blockReadsOutsideWorkingDirectories":true}}"#;
    let expected = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--json-schema",
        r#"{"type":"object"}"#,
        "--append-system-prompt-file",
        "/bundle/system.md",
        "--exclude-dynamic-system-prompt-sections",
        "--restricted",
        "--safe-mode",
        "--settings",
        settings,
        "--tools",
        "Read,Grep,Glob",
        "--add-dir",
        "/work/pr",
        "--permission-mode",
        "dontAsk",
        "--max-turns",
        "8",
        "--max-budget-usd",
        "0.74",
    ];
    assert_eq!(scene.argv()?, expected);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn the_prompt_arrives_on_stdin_and_not_as_an_argument() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let call = call()?;

    scene.runner("ok", false)?.run(&call)?;

    let argv = scene.argv()?;
    assert_eq!(
        fs::read_to_string(scene.record("stdin"))?,
        call.user_prompt.as_str()
    );
    assert_eq!(argv.get(1).map(String::as_str), Some("--output-format"));
    assert!(
        !argv.iter().any(|arg| arg == call.user_prompt.as_str()),
        "the prompt was passed as an argument"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn bare_mode_is_never_requested() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    scene.runner("ok", false)?.run(&call()?)?;

    assert!(
        !scene.argv()?.iter().any(|arg| arg == "--bare"),
        "--bare ignores the OAuth token (03 §8)"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn read_blocking_is_nested_under_permissions() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    scene.runner("ok", false)?.run(&call()?)?;

    let argv = scene.argv()?;
    let at = argv
        .iter()
        .position(|arg| arg == "--settings")
        .ok_or("no --settings")?;
    let settings: Value = serde_json::from_str(argv.get(at + 1).ok_or("no settings value")?)?;
    assert_eq!(
        settings.pointer("/permissions/blockReadsOutsideWorkingDirectories"),
        Some(&Value::Bool(true))
    );
    assert_eq!(settings.get("blockReadsOutsideWorkingDirectories"), None);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn every_call_runs_in_the_shared_empty_directory() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let runner = scene.runner("ok", false)?;
    let call = call()?;

    runner.run(&call)?;
    let first = fs::read_to_string(scene.record("cwd"))?;
    runner.run(&call)?;
    let second = fs::read_to_string(scene.record("cwd"))?;

    let expected = format!("{}\n", fs::canonicalize(scene.workdir())?.display());
    assert_eq!((first, second), (expected.clone(), expected));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn the_environment_holds_only_the_token_the_scrub_flag_and_passed_variables()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    scene.runner("ok", false)?.run(&call()?)?;

    let env = scene.env()?;
    assert_eq!(
        env.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
        Some(TOKEN)
    );
    assert_eq!(
        env.get("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB")
            .map(String::as_str),
        Some("1")
    );
    // The shell adds PWD, SHLVL and `_` itself; nothing else may come from this process.
    let expected = [
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
        "FAKE_RECORD",
        "FAKE_STREAM",
        "PWD",
        "SHLVL",
        "_",
    ];
    let unexpected: Vec<&String> = env
        .keys()
        .filter(|name| !expected.contains(&name.as_str()))
        .collect();
    assert_eq!(unexpected, Vec::<&String>::new());
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn the_stream_is_classified_with_its_cost() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    let result = scene.runner("budget_exhausted", false)?.run(&call()?)?;

    assert_eq!(
        (result.outcome, result.cost),
        (ModelOutcome::BudgetExhausted, Aeu::from_micros(751_200))
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn a_call_past_its_timeout_is_killed_and_charged_the_cap() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut call = call()?;
    call.timeout = Duration::from_millis(300);

    let result = scene.runner("partial", true)?.run(&call)?;

    // How much of the stream the stand-in wrote before the kill depends on scheduling, so only
    // the outcome and the charge are asserted here; `Replay` pins what a timed-out transcript
    // keeps.
    assert_eq!(
        (result.outcome, result.cost),
        (ModelOutcome::Timeout, BUDGET)
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-in claude; Miri cannot run processes")]
fn output_past_the_cap_is_malformed_and_charged_the_cap() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let runner = scene.runner("ok", false)?.with_max_output_bytes(64);

    let result = runner.run(&call()?)?;

    assert_eq!(
        (result.outcome, result.cost),
        (ModelOutcome::Malformed, BUDGET)
    );
    Ok(())
}

#[test]
fn a_working_directory_that_is_no_longer_empty_stops_the_call() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let runner = scene.runner("ok", false)?;
    fs::write(scene.workdir().join("planted"), "x")?;

    let failed = runner.run(&call()?);

    assert_matches!(
        failed,
        Err(HeadlessError::Workdir(WorkdirError::NotEmpty { .. }))
    );
    assert!(!scene.record("argv").exists(), "claude was started");
    Ok(())
}

#[test]
fn a_relative_path_in_the_call_stops_it() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut call = call()?;
    call.add_dir = PathBuf::from("pr");

    let failed = scene.runner("ok", false)?.run(&call);

    assert_matches!(failed, Err(HeadlessError::RelativePath { path }) if path == Path::new("pr"));
    assert!(!scene.record("argv").exists(), "claude was started");
    Ok(())
}

#[test]
fn a_timeout_past_the_clock_stops_the_call_before_claude_starts() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let mut call = call()?;
    call.timeout = Duration::MAX;

    let failed = scene.runner("ok", false)?.run(&call);

    assert_matches!(
        &failed,
        Err(error @ HeadlessError::TimeoutTooLong { .. }) if !error.may_have_run()
    );
    assert!(!scene.record("argv").exists(), "claude was started");
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "tries to spawn a process; Miri cannot run processes")]
fn a_program_that_cannot_start_is_a_spawn_error_that_never_ran() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let runner = ClaudeHeadless::new(
        scene.root.path().join("absent-claude"),
        Workdir::new(scene.workdir())?,
        HeadlessEnv::new(TOKEN),
    );

    let failed = runner.run(&call()?);

    assert_matches!(
        &failed,
        Err(error @ HeadlessError::Spawn(_)) if !error.may_have_run()
    );
    assert!(!scene.record("argv").exists(), "claude was started");
    Ok(())
}

#[test]
fn only_errors_after_the_start_may_have_run() {
    let io = || io::Error::other("injected");
    let before = [
        HeadlessError::RelativePath {
            path: PathBuf::from("pr"),
        },
        HeadlessError::Workdir(WorkdirError::NotEmpty {
            path: PathBuf::from("/empty"),
        }),
        HeadlessError::TimeoutTooLong {
            timeout: Duration::MAX,
        },
        HeadlessError::SpoolCreate(io()),
        HeadlessError::Spawn(io()),
    ];
    let after = [
        HeadlessError::Supervise(io()),
        HeadlessError::Kill(io()),
        HeadlessError::ReadBack(io()),
    ];

    for error in &before {
        assert!(!error.may_have_run(), "{error:?} may have run");
    }
    for error in &after {
        assert!(error.may_have_run(), "{error:?} cannot have run");
    }
}

#[test]
fn a_working_directory_must_be_absolute_and_empty() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    fs::write(scene.record("file"), "x")?;

    assert_matches!(
        Workdir::new("relative/dir"),
        Err(WorkdirError::NotAbsolute { .. })
    );
    assert_matches!(
        Workdir::new(scene.root.path().join("record")),
        Err(WorkdirError::NotEmpty { .. })
    );
    assert_matches!(
        Workdir::new(scene.root.path().join("absent")),
        Err(WorkdirError::Unreadable { .. })
    );
    Ok(())
}

#[test]
fn api_keys_and_cloud_providers_cannot_be_passed() {
    let mut env = HeadlessEnv::new(TOKEN);

    for name in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "AWS_ACCESS_KEY_ID",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "CLOUD_ML_REGION",
        "VERTEX_REGION_CLAUDE_SONNET_4_5",
        "NODE_OPTIONS",
        "CLAUDE_CONFIG_DIR",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
    ] {
        assert_matches!(
            env.pass(name, "x"),
            Err(EnvVarError::Reserved { .. }),
            "{name} was accepted"
        );
    }
}

#[test]
fn a_variable_name_must_be_a_plain_identifier() {
    let mut env = HeadlessEnv::new(TOKEN);

    for name in ["", "A=B", "A B", "1A"] {
        assert_matches!(
            env.pass(name, "x"),
            Err(EnvVarError::InvalidName { .. }),
            "{name:?} was accepted"
        );
    }
}

#[test]
fn the_token_never_appears_in_debug_output() {
    let mut env = HeadlessEnv::new(TOKEN);
    env.pass("HOME", "/home/secret-value")
        .expect("HOME is a plain, unreserved name");

    let shown = format!("{env:?}");

    assert!(!shown.contains(TOKEN), "token leaked: {shown}");
    assert!(!shown.contains("secret-value"), "value leaked: {shown}");
}
