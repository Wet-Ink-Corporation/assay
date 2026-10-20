//! `assay doctor --profile <plugins-toml> <claude-program>` (#84): the preflight and the profile
//! canary against `assay-runner`'s stand-in `fake-claude-profile`, the report with one line per
//! check and per flag, its exit codes, and every refusal before the call.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const TOKEN: &str = "sk-ant-oat01-Q2FuYXJ5VG9rZW4tZm9yLXRlc3Rz";

const RUNBOOK: &str =
    "https://github.com/Wet-Ink-Corporation/assay/blob/main/docs/operations/token-rotation.md";

/// `assay-runner`'s fixtures: the profile stand-in, its streams, statuses and the 2.1.296 pins.
fn runner_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../assay-runner/tests/fixtures")
}

/// A scratch directory with the stand-in `claude` and its knobs (`bin/`), and the base config.
struct Scene {
    root: TempDir,
}

impl Scene {
    fn new(knobs: &str) -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir(&bin)?;
        fs::copy(
            runner_fixtures().join("fake-claude-profile"),
            bin.join("claude"),
        )?;
        let fixture = |name: &str| {
            runner_fixtures()
                .join(name)
                .to_str()
                .map(str::to_owned)
                .ok_or("fixture path is not UTF-8")
        };
        fs::write(
            bin.join("knobs"),
            format!(
                "STREAM='{}'\nVERSION='{}'\nAUTH='{}'\n{knobs}\n",
                fixture("stream/init_2_1_296.jsonl")?,
                fixture("status/version_2_1_296.txt")?,
                fixture("status/auth_oauth_2_1_296.json")?,
            ),
        )?;
        fs::copy(
            runner_fixtures().join("plugins-2.1.296.toml"),
            root.path().join("plugins.toml"),
        )?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// `assay doctor --profile` on this scene's config and stand-in, with only `PATH`,
    /// `token` (when given) and `extra` in its environment.
    fn command(&self, token: Option<&str>, extra: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
        command
            .args(["doctor", "--profile"])
            .arg(self.path("plugins.toml"))
            .arg(self.path("bin/claude"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(extra.iter().copied());
        if let Some(token) = token {
            command.env("CLAUDE_CODE_OAUTH_TOKEN", token);
        }
        command
    }
}

/// What `assay doctor --profile` printed and how it exited.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn outcome(mut command: Command) -> Result<Outcome, Box<dyn Error>> {
    let output = command.output()?;
    Ok(Outcome {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    })
}

fn refused(stderr: &str) -> Outcome {
    Outcome {
        code: Some(2),
        stdout: String::new(),
        stderr: format!("assay: {stderr}\n"),
    }
}

/// The check lines of a run in which every check passes.
const CHECKS_OK: &str = "ok cli-minimum\nok preflight\nok profile-accepted\nok permission-mode\n\
                         ok stdin-prompt\nok system-prompt-file\nok hook-canary\nok fence-canary\n\
                         ok add-dir\nok outside-read\n";

/// The flag lines of a run in which every check passes.
const FLAGS_OK: [&str; 15] = [
    "ok flag -p (prompt on stdin): shown by stdin-prompt",
    "ok flag --output-format stream-json: shown by profile-accepted",
    "ok flag --verbose: shown by profile-accepted",
    "ok flag --json-schema: shown by stdin-prompt, system-prompt-file, add-dir",
    "ok flag --append-system-prompt-file: shown by system-prompt-file",
    "ok flag --exclude-dynamic-system-prompt-sections: accepted (profile-accepted), its effect is \
     not exercised",
    "ok flag --restricted: shown by profile-accepted, preflight, outside-read",
    "ok flag --safe-mode: shown by profile-accepted, hook-canary",
    "ok flag --settings: shown by fence-canary, outside-read, hook-canary",
    "ok flag --tools Read,Grep,Glob: shown by preflight",
    "ok flag --add-dir: shown by add-dir",
    "ok flag --permission-mode dontAsk: shown by permission-mode",
    "ok flag --max-turns: accepted (profile-accepted), its effect is not exercised",
    "ok flag --max-budget-usd: accepted (profile-accepted), its effect is not exercised",
    "ok flag --include-hook-events: accepted (hook-canary), its effect is not exercised",
];

fn lines(lines: &[&str]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[test]
fn a_cli_that_honours_its_flags_passes_and_exits_0() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = outcome(scene.command(Some(TOKEN), &[]))?;

    assert_eq!(
        outcome,
        Outcome {
            code: Some(0),
            stdout: format!("{CHECKS_OK}{}", lines(&FLAGS_OK)),
            stderr: String::new(),
        }
    );
    Ok(())
}

#[test]
fn a_cli_that_ignores_its_flags_fails_and_exits_1() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("IGNORE_FLAGS=1")?;

    let outcome = outcome(scene.command(Some(TOKEN), &[]))?;

    let mut expected = vec![
        "ok cli-minimum".to_owned(),
        "ok preflight".to_owned(),
        "ok profile-accepted".to_owned(),
        "ok permission-mode".to_owned(),
        "ok stdin-prompt".to_owned(),
        "ok system-prompt-file".to_owned(),
        "FAIL hook-canary: the \"user\" canary hook ran".to_owned(),
        format!(
            "FAIL fence-canary: the transcript holds the OAuth token or an encoding of it; \
             rotate the token: {RUNBOOK}"
        ),
        "ok add-dir".to_owned(),
        "FAIL outside-read: the outside file's word is in the transcript: a read outside the \
         working directories was allowed"
            .to_owned(),
    ];
    let failed_flags = [
        (6, "FAIL flag --restricted: outside-read failed"),
        (7, "FAIL flag --safe-mode: hook-canary failed"),
        (8, "FAIL flag --settings: fence-canary failed"),
        (14, "FAIL flag --include-hook-events: hook-canary failed"),
    ];
    let mut flags: Vec<String> = FLAGS_OK.map(str::to_owned).to_vec();
    for (index, line) in failed_flags {
        if let Some(flag) = flags.get_mut(index) {
            *flag = line.to_owned();
        }
    }
    expected.extend(flags);
    assert_eq!(
        (outcome.code, outcome.stderr.as_str()),
        (Some(1), ""),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        outcome.stdout,
        lines(&expected.iter().map(String::as_str).collect::<Vec<_>>())
    );
    Ok(())
}

#[test]
fn a_provider_variable_fails_the_preflight_and_runs_no_canary() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = outcome(scene.command(Some(TOKEN), &[("ANTHROPIC_API_KEY", "")]))?;

    let report: Vec<&str> = outcome.stdout.lines().take(4).collect();
    assert_eq!(outcome.code, Some(1), "{}", outcome.stderr);
    assert_eq!(
        report,
        [
            "ok cli-minimum",
            "FAIL preflight: \"ANTHROPIC_API_KEY\" is set; credential and provider variables \
             must be unset",
            "FAIL profile-accepted: not run: the preflight failed",
            "FAIL permission-mode: not run: the preflight failed",
        ]
    );
    assert!(!scene.path("bin/argv").exists(), "the canary did not run");
    Ok(())
}

#[test]
fn a_cli_that_cannot_start_fails_the_preflight_with_every_cause() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let mut absent = Command::new(env!("CARGO_BIN_EXE_assay"));
    absent
        .args(["doctor", "--profile"])
        .arg(scene.path("plugins.toml"))
        .arg(scene.path("bin/absent"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_CODE_OAUTH_TOKEN", TOKEN);

    let outcome = outcome(absent)?;

    assert_eq!(outcome.code, Some(1), "{}", outcome.stderr);
    assert_eq!(
        outcome.stdout.lines().nth(1),
        Some(
            "FAIL preflight: the CLI's version and auth status could not be read: the claude CLI \
             could not be started: No such file or directory (os error 2)"
        )
    );
    Ok(())
}

#[test]
fn a_wrong_argument_count_is_a_usage_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let usage = refused("usage: assay doctor --profile <plugins-toml> <claude-program>");
    let run = |args: &[&Path]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
        command
            .args(["doctor", "--profile"])
            .args(args)
            .env_clear()
            .env("CLAUDE_CODE_OAUTH_TOKEN", TOKEN);
        outcome(command)
    };

    assert_eq!(run(&[])?, usage);
    assert_eq!(run(&[&scene.path("plugins.toml")])?, usage);
    assert_eq!(
        run(&[
            &scene.path("plugins.toml"),
            &scene.path("bin/claude"),
            Path::new("x")
        ])?,
        usage
    );
    Ok(())
}

#[test]
fn a_relative_program_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
    command
        .args(["doctor", "--profile"])
        .arg(scene.path("plugins.toml"))
        .arg("claude")
        .env_clear()
        .env("CLAUDE_CODE_OAUTH_TOKEN", TOKEN);

    assert_eq!(
        outcome(command)?,
        refused("the claude program claude is not an absolute path")
    );
    Ok(())
}

#[test]
fn a_missing_token_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = outcome(scene.command(None, &[]))?;

    assert_eq!(outcome, refused("CLAUDE_CODE_OAUTH_TOKEN is not set"));
    assert!(!scene.path("bin/argv").exists());
    Ok(())
}

#[test]
fn a_token_too_short_to_scan_for_is_refused_without_echoing_it() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = outcome(scene.command(Some("sk-short"), &[]))?;

    assert_eq!(
        outcome,
        refused(
            "CLAUDE_CODE_OAUTH_TOKEN cannot be scanned for: the token has fewer than 12 \
             alphanumeric characters"
        )
    );
    assert!(!scene.path("bin/argv").exists());
    Ok(())
}

#[test]
fn a_config_without_a_cli_pin_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    fs::write(
        scene.path("plugins.toml"),
        format!("[assay]\ndigest = \"sha256:{}\"\n", "0".repeat(64)),
    )?;

    let outcome = outcome(scene.command(Some(TOKEN), &[]))?;

    assert_eq!(
        outcome,
        refused("plugins.toml pins no claude_cli, so there is no CLI to check")
    );
    Ok(())
}
