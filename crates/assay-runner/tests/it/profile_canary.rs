//! `ClaudeCanary` as the profile canary against a stand-in `claude`
//! (`tests/fixtures/fake-claude-profile`): the planted hooks and files, the 03 §8 argv with
//! `--include-hook-events`, and what `verify_profile` concludes when the CLI honours or ignores
//! its flags (#84).

use std::assert_matches;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use assay_app::{
    ArtifactNonce, EgressToken, FENCE_TARGET, ModelOutcome, NonceSource, PROFILE_SCHEMA,
    ParseNonceError, PermissionDenial, PluginsConfig, ProfileCheck, ProfileContext, ProfileFailure,
    ProfilePorts, ProfileProbe, ProfileReport, ProfileRun, ProfileWords, ReplyField, Structured,
    verify_profile,
};
use assay_runner::{
    CanaryError, ClaudeCanary, ClaudeHeadless, HeadlessEnv, HeadlessError, StatusError, Workdir,
    read_session_init,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::{RecordingKill, fixtures_dir, stream, stream_path};

const TOKEN: &str = "sk-ant-oat01-Q2FuYXJ5VG9rZW4tZm9yLXRlc3Rz";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const PROMPT_WORD: &str = "ASSAYPROMPT0123456789abcdef0123456789abcdef";
const SYSTEM_WORD: &str = "ASSAYSYSTEM0123456789abcdef0123456789abcdef";
const TREE_WORD: &str = "ASSAYTREE0123456789abcdef0123456789abcdef";
const OUTSIDE_WORD: &str = "ASSAYOUTSIDE0123456789abcdef0123456789abcdef";
const SCOPES: [&str; 3] = ["user", "project", "tree"];

struct FixedNonce;

impl NonceSource for FixedNonce {
    type Error = ParseNonceError;

    fn nonce(&mut self) -> Result<ArtifactNonce, ParseNonceError> {
        NONCE.parse()
    }
}

/// A scratch directory holding the stand-in with its knobs (`bin/`), the directory sandboxes
/// are made in (`sandboxes/`) and the preflight's empty working directory (`empty/`).
struct Scene {
    root: TempDir,
    killer: RecordingKill,
}

impl Scene {
    /// A stand-in that honours its flags, with `knobs` as extra shell assignments.
    fn new(knobs: &str) -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        for dir in ["bin", "sandboxes", "empty"] {
            fs::create_dir(root.path().join(dir))?;
        }
        fs::copy(
            fixtures_dir().join("fake-claude-profile"),
            bin.join("claude"),
        )?;
        let utf8 = |path: PathBuf| path.to_str().map(str::to_owned).ok_or("not UTF-8");
        fs::write(
            bin.join("knobs"),
            format!(
                "STREAM='{}'\nVERSION='{}'\nAUTH='{}'\n{knobs}\n",
                utf8(stream_path("init_2_1_296"))?,
                utf8(fixtures_dir().join("status/version_2_1_296.txt"))?,
                utf8(fixtures_dir().join("status/auth_oauth_2_1_296.json"))?,
            ),
        )?;
        Ok(Self {
            root,
            killer: RecordingKill::default(),
        })
    }

    fn program(&self) -> PathBuf {
        self.root.path().join("bin/claude")
    }

    fn canary(&self) -> ClaudeCanary<&RecordingKill> {
        ClaudeCanary::new(self.program(), HeadlessEnv::new(TOKEN), &self.killer)
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

    fn report(&self) -> Result<ProfileReport<StatusError>, Box<dyn Error>> {
        let config: PluginsConfig =
            fs::read_to_string(fixtures_dir().join("plugins-2.1.296.toml"))?.parse()?;
        let assay = config.assay();
        let pinned = assay.claude_cli.as_ref().ok_or("no CLI pin")?;
        let token = EgressToken::new(TOKEN)?;
        let cli = ClaudeHeadless::new(
            self.program(),
            Workdir::new(self.root.path().join("empty"))?,
            HeadlessEnv::new(TOKEN),
            &self.killer,
        );
        let ports = ProfilePorts {
            cli: &cli,
            probe: &self.canary(),
            nonces: &mut FixedNonce,
        };
        let context = ProfileContext {
            pinned,
            allowlist: &assay.claude_cli_builtins,
            token: &token,
        };
        Ok(verify_profile(["HOME", "PATH"], ports, &context)?)
    }

    fn run(&self) -> Result<ProfileRun, Box<dyn Error>> {
        Ok(self.canary().probe_profile(&words()?)?)
    }
}

fn words() -> Result<ProfileWords, Box<dyn Error>> {
    let nonce = NONCE.parse()?;
    Ok(ProfileWords::new([&nonce, &nonce, &nonce, &nonce]))
}

/// Each check's failure, in report order.
fn failures(report: &ProfileReport<StatusError>) -> Vec<(ProfileCheck, Option<String>)> {
    report
        .results()
        .iter()
        .map(|result| {
            (
                result.check,
                result.failure.as_ref().map(ToString::to_string),
            )
        })
        .collect()
}

/// The failure of `check` in the report for a stand-in with `knobs`, shown as text; an error
/// when the check passed.
fn failure_of(knobs: &str, check: ProfileCheck) -> Result<String, Box<dyn Error>> {
    let report = Scene::new(knobs)?.report()?;
    let failure = report
        .results()
        .iter()
        .find(|result| result.check == check)
        .ok_or("check missing")?
        .failure
        .as_ref()
        .ok_or_else(|| format!("{check} passed"))?;
    Ok(failure.to_string())
}

fn shown(failure: &ProfileFailure<StatusError>) -> String {
    failure.to_string()
}

#[test]
fn a_cli_that_honours_its_flags_passes_every_profile_check() -> Result<(), Box<dyn Error>> {
    let report = Scene::new("")?.report()?;

    assert_eq!(
        failures(&report),
        ProfileCheck::ALL.map(|check| (check, None)).to_vec()
    );
    let failed_flags: Vec<ProfileCheck> = report.flags().filter_map(|flag| flag.failed).collect();
    assert_eq!(failed_flags, []);
    Ok(())
}

#[test]
fn the_run_reports_what_the_call_showed() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let run = scene.run()?;

    let sandbox = scene.sandbox()?;
    let path = |name: &str| sandbox.join(name).display().to_string();
    assert_eq!(run.init, read_session_init(&stream("init_2_1_296")?));
    assert_eq!(
        run.outcome,
        ModelOutcome::Ok(Structured::new(json!({
            "prompt_word": PROMPT_WORD,
            "system_word": SYSTEM_WORD,
            "tree_word": TREE_WORD,
        })))
    );
    assert_eq!(run.permission_mode.as_deref(), Some("dontAsk"));
    let read = |file_path: String| PermissionDenial {
        tool_name: "Read".to_owned(),
        file_path: Some(file_path),
    };
    assert_eq!(
        run.denials,
        Some(vec![
            read("/proc/self/environ".to_owned()),
            read(path("outside/OUTSIDE.md")),
        ])
    );
    assert_eq!(run.hook_events, Vec::<String>::new());
    assert_eq!(run.planted_hooks, SCOPES);
    assert_eq!(run.fired_hooks, Vec::<String>::new());
    assert_eq!(run.tree_file, path("pr/TREE.md"));
    assert_eq!(run.outside_file, path("outside/OUTSIDE.md"));
    Ok(())
}

#[test]
fn plants_a_hook_at_each_scope_and_the_tree_and_outside_files() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.run()?;

    let settings = "./.claude/settings.json\n";
    assert_eq!(scene.record("home-files")?, settings);
    assert_eq!(scene.record("cwd-files")?, settings);
    assert_eq!(scene.record("pr-files")?, format!("{settings}./TREE.md\n"));
    assert_eq!(scene.record("outside-files")?, "./OUTSIDE.md\n");
    Ok(())
}

#[test]
fn each_hook_writes_its_own_marker_on_every_event() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.run()?;

    let markers = scene.sandbox()?.join("markers");
    let expected: Vec<String> = SCOPES
        .iter()
        .map(|scope| markers.join(format!("hook-{scope}")).display().to_string())
        .collect();
    let recorded: Vec<String> = scene
        .record("markers")?
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(recorded, expected);
    let settings: Vec<Value> = scene
        .record("settings")?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    for (settings, marker) in settings.iter().zip(&expected) {
        let command = json!({ "type": "command", "command": format!(": > '{marker}'") });
        let every = json!([{ "hooks": [command] }]);
        let tools = json!([{ "matcher": "*", "hooks": [command] }]);
        assert_eq!(
            settings,
            &json!({ "hooks": {
                "SessionStart": every,
                "UserPromptSubmit": every,
                "PreToolUse": tools,
                "PostToolUse": tools,
                "Stop": every,
                "SessionEnd": every,
            }})
        );
    }
    Ok(())
}

#[test]
fn the_words_are_planted_where_each_flag_must_carry_them() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.run()?;

    let sandbox = scene.sandbox()?;
    let tree = sandbox.join("pr/TREE.md").display().to_string();
    let outside = sandbox.join("outside/OUTSIDE.md").display().to_string();
    let stdin = scene.record("stdin")?;
    for wanted in [
        format!("Read {FENCE_TARGET}"),
        format!("Read {tree}"),
        format!("Read {outside}"),
        format!("prompt_word to `{PROMPT_WORD}`"),
    ] {
        assert!(stdin.contains(&wanted), "{wanted:?} not in {stdin:?}");
    }
    for word in [SYSTEM_WORD, TREE_WORD, OUTSIDE_WORD] {
        assert!(!stdin.contains(word), "{word} in the prompt");
    }
    assert_eq!(
        [
            scene.record("system.md")?,
            scene.record("tree.md")?,
            scene.record("outside.md")?
        ],
        [
            format!(
                "You are checking that a session starts and reads files as asked. \
                 The system word is `{SYSTEM_WORD}`.\n"
            ),
            format!("The tree word is `{TREE_WORD}`.\n"),
            format!("The outside word is `{OUTSIDE_WORD}`.\n"),
        ]
    );
    Ok(())
}

#[test]
fn runs_the_production_argv_with_hook_events_inside_the_sandbox() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.run()?;

    let sandbox = scene.sandbox()?;
    let path = |name: &str| sandbox.join(name).display().to_string();
    let settings = r#"{"disableAllHooks":true,"permissions":{"deny":["Read(//proc/**)","Read(//sys/**)","Read(~/**)","Read(//etc/**)"],"blockReadsOutsideWorkingDirectories":true}}"#;
    let expected = [
        "-p".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        "--verbose".to_owned(),
        "--json-schema".to_owned(),
        PROFILE_SCHEMA.to_owned(),
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
        "4".to_owned(),
        "--max-budget-usd".to_owned(),
        "0.25".to_owned(),
        "--include-hook-events".to_owned(),
    ];
    let argv: Vec<String> = scene
        .record("argv")?
        .split_terminator('\0')
        .map(str::to_owned)
        .collect();
    assert_eq!(argv, expected);
    assert_eq!(scene.record("cwd")?, format!("{}\n", path("cwd")));
    let env = scene.record("env")?;
    assert!(env.contains(&format!("HOME={}\n", path("home"))), "{env}");
    assert!(
        env.contains(&format!("CLAUDE_CODE_OAUTH_TOKEN={TOKEN}\n")),
        "{env}"
    );
    Ok(())
}

#[test]
fn the_system_prompt_file_gives_the_system_word() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let run = scene.run()?;

    let ModelOutcome::Ok(structured) = run.outcome else {
        return Err("no answer".into());
    };
    assert_eq!(
        structured.as_value()[ReplyField::System.key()],
        Value::from(SYSTEM_WORD),
        "the stand-in read it back from the planted file"
    );
    Ok(())
}

#[test]
fn the_sandbox_is_removed_after_the_run() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    scene.run()?;

    assert!(!scene.sandbox()?.exists());
    assert_eq!(
        fs::read_dir(scene.root.path().join("sandboxes"))?.count(),
        0
    );
    Ok(())
}

#[test]
fn hooks_that_ran_are_reported_and_fail_the_hook_canary() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("RUN_HOOKS=1")?;

    let run = scene.run()?;

    assert_eq!(run.fired_hooks, SCOPES);
    assert_eq!(
        failure_of("RUN_HOOKS=1", ProfileCheck::HookCanary)?,
        shown(&ProfileFailure::HookRan {
            scope: "user".to_owned()
        })
    );
    Ok(())
}

#[test]
fn a_streamed_hook_event_fails_the_hook_canary() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        failure_of("HOOK_EVENTS=1", ProfileCheck::HookCanary)?,
        shown(&ProfileFailure::HookEvent {
            event: "SessionStart".to_owned()
        })
    );
    Ok(())
}

#[test]
fn an_environ_read_let_through_fails_the_fence() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        failure_of("READ_ENVIRON=1", ProfileCheck::FenceCanary)?,
        shown(&ProfileFailure::TokenInTranscript)
    );
    Ok(())
}

#[test]
fn an_outside_read_let_through_fails() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        failure_of("READ_OUTSIDE=1", ProfileCheck::OutsideRead)?,
        shown(&ProfileFailure::OutsideRead)
    );
    Ok(())
}

#[test]
fn a_denied_tree_read_fails_the_add_dir_check() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("DENY_TREE=1")?;

    let report = scene.report()?;

    let tree = scene.sandbox()?.join("pr/TREE.md").display().to_string();
    let add_dir = report
        .results()
        .iter()
        .find(|result| result.check == ProfileCheck::AddDir)
        .and_then(|result| result.failure.as_ref())
        .map(ToString::to_string);
    assert_eq!(
        add_dir,
        Some(shown(&ProfileFailure::TreeDenied { path: tree }))
    );
    Ok(())
}

#[test]
fn another_permission_mode_fails() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        failure_of("PERMISSION_MODE=default", ProfileCheck::PermissionMode)?,
        shown(&ProfileFailure::PermissionMode {
            found: Some("default".to_owned())
        })
    );
    Ok(())
}

#[test]
fn a_prompt_or_system_word_not_carried_fails_its_check() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        failure_of("IGNORE_STDIN=1", ProfileCheck::StdinPrompt)?,
        shown(&ProfileFailure::WordMissing {
            field: ReplyField::Prompt
        })
    );
    assert_eq!(
        failure_of("IGNORE_SYSTEM=1", ProfileCheck::SystemPromptFile)?,
        shown(&ProfileFailure::WordMissing {
            field: ReplyField::System
        })
    );
    Ok(())
}

#[test]
fn a_refused_argv_fails_profile_accepted_naming_the_fallback() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        failure_of("REFUSE=1", ProfileCheck::ProfileAccepted)?,
        shown(&ProfileFailure::ProfileRefused {
            outcome: "malformed"
        })
    );
    Ok(())
}

/// The negative test: a CLI whose flags are accepted but ineffective runs the planted hooks,
/// streams their events, and lets the environ and outside reads through.
#[test]
fn a_cli_that_ignores_its_flags_fails_the_hook_fence_and_outside_checks()
-> Result<(), Box<dyn Error>> {
    let report = Scene::new("IGNORE_FLAGS=1")?.report()?;

    let failed: Vec<ProfileCheck> = report
        .results()
        .iter()
        .filter(|result| result.failure.is_some())
        .map(|result| result.check)
        .collect();
    assert_eq!(
        failed,
        [
            ProfileCheck::HookCanary,
            ProfileCheck::FenceCanary,
            ProfileCheck::OutsideRead
        ]
    );
    assert!(!report.passed());
    Ok(())
}

#[test]
fn a_cli_that_cannot_start_is_a_run_error_and_leaves_no_sandbox() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let canary = ClaudeCanary::new(
        scene.root.path().join("bin/absent"),
        HeadlessEnv::new(TOKEN),
        &scene.killer,
    )
    .in_dir(scene.root.path().join("sandboxes"));

    let probed = canary.probe_profile(&words()?);

    assert_matches!(probed, Err(CanaryError::Run(HeadlessError::Spawn(_))));
    assert_eq!(
        fs::read_dir(scene.root.path().join("sandboxes"))?.count(),
        0
    );
    Ok(())
}
