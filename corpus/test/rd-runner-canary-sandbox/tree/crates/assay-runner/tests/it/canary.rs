//! `ClaudeCanary` against a stand-in `claude` (`tests/fixtures/fake-claude-canary`): the planted
//! throwaway checkout, the 03 §8 invocation inside it, and what `doctor` concludes when the CLI
//! honours or ignores its safety flags (#121).

use std::assert_matches;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use assay_app::{
    ArtifactNonce, CanaryProbe, CanaryToken, Check, CheckFailure, DoctorReport, ModelOutcome,
    NonceSource, ParseNonceError, PluginsConfig, Structured, doctor,
};
use assay_runner::{CanaryError, ClaudeCanary, HeadlessEnv, HeadlessError, read_session_init};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::{RecordingKill, fixtures_dir, stream, stream_path};

const TOKEN: &str = "sk-ant-oat01-test-token";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const WORD: &str = "ASSAYCANARY0123456789abcdef0123456789abcdef";
const MODS: [&str; 3] = [
    "assay-canary-user",
    "assay-canary-project",
    "assay-canary-tree",
];

struct FixedNonce;

impl NonceSource for FixedNonce {
    type Error = ParseNonceError;

    fn nonce(&mut self) -> Result<ArtifactNonce, ParseNonceError> {
        NONCE.parse()
    }
}

/// A scratch directory holding the stand-in with its knobs (`bin/`) and the directory canary
/// sandboxes are made in (`sandboxes/`).
struct Scene {
    root: TempDir,
    killer: RecordingKill,
}

impl Scene {
    /// A stand-in that prints the 2.1.296 recording, with `knobs` as extra shell assignments.
    fn new(knobs: &str) -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir(&bin)?;
        fs::create_dir(root.path().join("sandboxes"))?;
        fs::copy(
            fixtures_dir().join("fake-claude-canary"),
            bin.join("claude"),
        )?;
        let stream = stream_path("init_2_1_296");
        let stream = stream.to_str().ok_or("fixture path is not UTF-8")?;
        fs::write(bin.join("knobs"), format!("STREAM='{stream}'\n{knobs}\n"))?;
        Ok(Self {
            root,
            killer: RecordingKill::default(),
        })
    }

    fn canary(&self) -> ClaudeCanary<&RecordingKill> {
        ClaudeCanary::new(
            self.root.path().join("bin/claude"),
            HeadlessEnv::new(TOKEN),
            &self.killer,
        )
        .in_dir(self.root.path().join("sandboxes"))
    }

    /// What the stand-in recorded as `name`.
    fn record(&self, name: &str) -> Result<String, Box<dyn Error>> {
        Ok(fs::read_to_string(self.root.path().join("bin").join(name))?)
    }

    /// The sandbox the call ran in: the parent of its working directory.
    fn sandbox(&self) -> Result<PathBuf, Box<dyn Error>> {
        let cwd = PathBuf::from(self.record("cwd")?.trim_end());
        Ok(cwd.parent().ok_or("cwd has no parent")?.to_path_buf())
    }

    fn doctor(&self) -> Result<DoctorReport, Box<dyn Error>> {
        let config: PluginsConfig =
            fs::read_to_string(fixtures_dir().join("plugins-2.1.296.toml"))?.parse()?;
        let assay = config.assay();
        let pinned = assay.claude_cli.as_ref().ok_or("no CLI pin")?;
        Ok(doctor(
            &self.canary(),
            &mut FixedNonce,
            pinned,
            &assay.claude_cli_builtins,
        )?)
    }
}

fn token() -> Result<CanaryToken, Box<dyn Error>> {
    Ok(CanaryToken::new(&NONCE.parse()?))
}

/// Each check's failure, in report order.
fn failures(report: &DoctorReport) -> Vec<(Check, Option<CheckFailure>)> {
    report
        .results()
        .iter()
        .map(|result| (result.check, result.failure.clone()))
        .collect()
}

#[test]
fn a_cli_that_honours_its_flags_passes_every_doctor_check() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let report = scene.doctor()?;

    assert_eq!(
        failures(&report),
        [
            (Check::CleanInit, None),
            (Check::BuiltinAllowlist, None),
            (Check::ModCanary, None),
            (Check::AgentsMdCanary, None),
        ]
    );
    Ok(())
}

#[test]
fn the_run_reports_the_init_outcome_and_planted_mods() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let run = scene.canary().probe(&token()?)?;

    assert_eq!(run.init, read_session_init(&stream("init_2_1_296")?));
    assert_eq!(
        run.outcome,
        ModelOutcome::Ok(Structured::new(json!({ "reply": "ready" })))
    );
    assert_eq!(run.transcript, stream("init_2_1_296")?);
    assert_eq!(run.planted_mods, MODS);
    assert_eq!(run.loaded_mods, Vec::<String>::new());
    Ok(())
}

#[test]
fn plants_a_user_scope_mod_and_skills_folder_mods_with_the_instructions()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.canary().probe(&token()?)?;

    let mod_files = |dir: &str| {
        [
            format!("./{dir}/.claude-plugin/plugin.json"),
            format!("./{dir}/hooks/hooks.json"),
            format!("./{dir}/hooks/register.ts"),
        ]
    };
    let mut home: Vec<String> = vec!["./.claude/settings.json".to_owned()];
    home.extend(mod_files("mods/assay-canary-user"));
    let mut cwd: Vec<String> = vec!["./AGENTS.md".to_owned(), "./CLAUDE.md".to_owned()];
    cwd.extend(mod_files(".claude/skills/assay-canary-project"));
    let mut pr: Vec<String> = vec!["./AGENTS.md".to_owned(), "./CLAUDE.md".to_owned()];
    pr.extend(mod_files(".claude/skills/assay-canary-tree"));
    let sorted = |mut files: Vec<String>| {
        files.sort();
        let mut text = files.join("\n");
        text.push('\n');
        text
    };
    assert_eq!(scene.record("home-files")?, sorted(home));
    assert_eq!(scene.record("cwd-files")?, sorted(cwd));
    assert_eq!(scene.record("pr-files")?, sorted(pr));
    Ok(())
}

#[test]
fn the_user_scope_mod_is_named_in_the_home_settings() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.canary().probe(&token()?)?;

    let settings: Value = serde_json::from_str(&scene.record("settings.json")?)?;
    let user_mod = scene.sandbox()?.join("home/mods/assay-canary-user");
    assert_eq!(
        settings,
        json!({ "env": { "CLAUDE_CODE_PLUGIN_DIRS": user_mod.to_str().ok_or("not UTF-8")? } })
    );
    Ok(())
}

#[test]
fn each_mod_writes_its_own_marker_in_the_sandbox() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.canary().probe(&token()?)?;

    let markers = scene.sandbox()?.join("markers");
    let mut expected: Vec<String> = MODS
        .iter()
        .map(|name| markers.join(name).display().to_string())
        .collect();
    expected.sort();
    let mut recorded: Vec<String> = scene
        .record("markers")?
        .lines()
        .map(str::to_owned)
        .collect();
    recorded.sort();
    assert_eq!(recorded, expected);
    Ok(())
}

#[test]
fn the_instructions_hold_the_word_only_in_two_parts() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.canary().probe(&token()?)?;

    let agents_md = scene.record("AGENTS.md")?;
    assert!(
        agents_md.contains(&format!("`ASSAYCANARY` immediately followed by `{NONCE}`")),
        "{agents_md}"
    );
    assert!(!agents_md.contains(WORD), "{agents_md}");
    Ok(())
}

#[test]
fn runs_the_headless_profile_inside_the_sandbox() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.canary().probe(&token()?)?;

    let sandbox = scene.sandbox()?;
    let path = |name: &str| sandbox.join(name).display().to_string();
    let schema = r#"{"type":"object","properties":{"reply":{"type":"string"}},"required":["reply"],"additionalProperties":false}"#;
    let settings = r#"{"disableAllHooks":true,"permissions":{"deny":["Read(//proc/**)","Read(//sys/**)","Read(~/**)","Read(//etc/**)"],"blockReadsOutsideWorkingDirectories":true}}"#;
    let expected = [
        "-p".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--verbose".to_owned(),
        "--json-schema".to_owned(),
        schema.to_owned(),
        "--append-system-prompt-file".to_owned(),
        path("prompt/system.md"),
        "--exclude-dynamic-system-prompt-sections".to_owned(),
        "--restricted".to_owned(),
        "--safe-mode".to_owned(),
        "--settings".to_owned(),
        settings.to_owned(),
        "--tools".to_owned(),
        "Read,Grep,Glob".to_owned(),
        "--add-dir".to_owned(),
        path("pr"),
        "--permission-mode".to_owned(),
        "dontAsk".to_owned(),
        "--max-turns".to_owned(),
        "2".to_owned(),
        "--max-budget-usd".to_owned(),
        "0.25".to_owned(),
    ];
    let argv: Vec<String> = scene
        .record("argv")?
        .split_terminator('\0')
        .map(str::to_owned)
        .collect();
    assert_eq!(argv, expected);
    assert_eq!(scene.record("cwd")?, format!("{}\n", path("cwd")));
    assert_eq!(scene.record("stdin")?, "Reply with the single word ready.");
    let env = scene.record("env")?;
    assert!(env.contains(&format!("HOME={}\n", path("home"))), "{env}");
    assert!(
        env.contains(&format!("CLAUDE_CODE_OAUTH_TOKEN={TOKEN}\n")),
        "{env}"
    );
    Ok(())
}

#[test]
fn the_sandbox_is_removed_after_the_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.canary().probe(&token()?)?;

    assert!(!scene.sandbox()?.exists());
    assert_eq!(
        fs::read_dir(scene.root.path().join("sandboxes"))?.count(),
        0
    );
    Ok(())
}

#[test]
fn mods_that_ran_are_reported_loaded() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("RUN_MODS=1")?;

    let run = scene.canary().probe(&token()?)?;

    assert_eq!(run.loaded_mods, MODS);
    Ok(())
}

#[test]
fn a_mod_that_ran_fails_the_mod_canary() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("RUN_MODS=1")?;

    let report = scene.doctor()?;

    assert_eq!(
        failures(&report).get(2),
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
fn a_followed_instruction_fails_the_agents_md_canary() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("FOLLOW_AGENTS=1")?;

    let report = scene.doctor()?;

    assert_eq!(
        failures(&report).get(3),
        Some(&(
            Check::AgentsMdCanary,
            Some(CheckFailure::InstructionFollowed)
        ))
    );
    Ok(())
}

/// The negative test: a CLI whose safety flags are accepted but ineffective (as if removed)
/// loads the mods and follows AGENTS.md, and both canaries fail.
#[test]
fn a_cli_that_ignores_its_safety_flags_fails_both_canaries() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("RUN_MODS=1\nFOLLOW_AGENTS=1")?;

    let report = scene.doctor()?;

    assert!(!report.passed());
    assert_eq!(
        failures(&report),
        [
            (Check::CleanInit, None),
            (Check::BuiltinAllowlist, None),
            (
                Check::ModCanary,
                Some(CheckFailure::ModLoaded {
                    name: "assay-canary-user".to_owned()
                })
            ),
            (
                Check::AgentsMdCanary,
                Some(CheckFailure::InstructionFollowed)
            ),
        ]
    );
    Ok(())
}

#[test]
fn a_cli_that_cannot_start_is_a_run_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let canary = ClaudeCanary::new(
        scene.root.path().join("bin/absent"),
        HeadlessEnv::new(TOKEN),
        &scene.killer,
    )
    .in_dir(scene.root.path().join("sandboxes"));

    let probed = canary.probe(&token()?);

    assert_matches!(probed, Err(CanaryError::Run(HeadlessError::Spawn(_))));
    Ok(())
}

#[test]
fn markers_that_cannot_be_inspected_are_an_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("BREAK_MARKERS=1")?;

    let probed = scene.canary().probe(&token()?);

    assert_matches!(probed, Err(CanaryError::Inspect(_)));
    Ok(())
}

#[test]
fn a_sandbox_that_cannot_be_removed_is_an_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("REMOVE_SANDBOX=1")?;

    let probed = scene.canary().probe(&token()?);

    assert_matches!(probed, Err(CanaryError::Cleanup(_)));
    Ok(())
}

#[test]
fn a_sandbox_that_cannot_be_made_is_an_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let canary = scene.canary().in_dir(scene.root.path().join("absent"));

    let probed = canary.probe(&token()?);

    assert_matches!(probed, Err(CanaryError::Sandbox(_)));
    assert!(!scene.root.path().join("bin/argv").exists());
    Ok(())
}

#[test]
fn a_relative_sandbox_directory_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let canary = scene.canary().in_dir("sandboxes");

    let probed = canary.probe(&token()?);

    assert_matches!(
        probed,
        Err(CanaryError::RelativeDir { ref path }) if path == Path::new("sandboxes")
    );
    Ok(())
}

#[test]
fn a_sandbox_path_that_is_not_utf8_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let parent = scene.root.path().join(OsStr::from_bytes(b"sand\xffboxes"));
    fs::create_dir(&parent)?;
    let canary = scene.canary().in_dir(&parent);

    let probed = canary.probe(&token()?);

    assert_matches!(probed, Err(CanaryError::NotUtf8 { .. }));
    assert!(!scene.root.path().join("bin/argv").exists());
    Ok(())
}
