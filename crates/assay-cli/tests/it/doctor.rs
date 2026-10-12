//! `assay doctor <plugins-toml> <claude-program>` (#121): the canary call against a stand-in
//! `claude` (`assay-runner`'s `tests/fixtures/fake-claude-canary`), its report and exit codes,
//! and every refusal before the call.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// `assay-runner`'s fixtures: the canary stand-in, its streams and the 2.1.296 pins.
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
            runner_fixtures().join("fake-claude-canary"),
            bin.join("claude"),
        )?;
        let stream = runner_fixtures().join("stream/init_2_1_296.jsonl");
        let stream = stream.to_str().ok_or("fixture path is not UTF-8")?;
        fs::write(bin.join("knobs"), format!("STREAM='{stream}'\n{knobs}\n"))?;
        fs::copy(
            runner_fixtures().join("plugins-2.1.296.toml"),
            root.path().join("plugins.toml"),
        )?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn program(&self) -> PathBuf {
        self.path("bin/claude")
    }
}

/// What `assay doctor` printed and how it exited.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn doctor(args: &[&Path], token: Option<&str>) -> Result<Outcome, Box<dyn Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_assay"));
    command
        .arg("doctor")
        .args(args)
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN");
    if let Some(token) = token {
        command.env("CLAUDE_CODE_OAUTH_TOKEN", token);
    }
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

#[test]
fn a_clean_session_passes_and_exits_0() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = doctor(&[&scene.path("plugins.toml"), &scene.program()], Some("t"))?;

    assert_eq!(
        outcome,
        Outcome {
            code: Some(0),
            stdout: "ok clean-init\nok builtin-allowlist\nok mod-canary\nok agents-md-canary\n"
                .to_owned(),
            stderr: String::new(),
        }
    );
    Ok(())
}

#[test]
fn a_cli_that_ignores_its_safety_flags_fails_and_exits_1() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("RUN_MODS=1\nFOLLOW_AGENTS=1")?;

    let outcome = doctor(&[&scene.path("plugins.toml"), &scene.program()], Some("t"))?;

    assert_eq!(
        outcome,
        Outcome {
            code: Some(1),
            stdout: "ok clean-init\n\
                     ok builtin-allowlist\n\
                     FAIL mod-canary: mod \"assay-canary-user\" ran its session.start hook\n\
                     FAIL agents-md-canary: the model followed the planted AGENTS.md/CLAUDE.md \
                     instruction\n"
                .to_owned(),
            stderr: String::new(),
        }
    );
    Ok(())
}

#[test]
fn drift_from_the_pinned_allowlist_fails() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let pins =
        fs::read_to_string(scene.path("plugins.toml"))?.replace("\"Plan\"", "\"Plan\", \"Old\"");
    fs::write(scene.path("plugins.toml"), pins)?;

    let outcome = doctor(&[&scene.path("plugins.toml"), &scene.program()], Some("t"))?;

    assert_eq!(outcome.code, Some(1));
    assert_eq!(
        outcome.stdout.lines().nth(1),
        Some(
            "FAIL builtin-allowlist: the observed built-ins differ from claude_cli_builtins: - agent \"Old\""
        )
    );
    Ok(())
}

#[test]
fn a_wrong_argument_count_is_a_usage_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let usage = refused("usage: assay doctor <plugins-toml> <claude-program>");

    assert_eq!(doctor(&[&scene.path("plugins.toml")], Some("t"))?, usage);
    assert_eq!(
        doctor(
            &[
                &scene.path("plugins.toml"),
                &scene.program(),
                Path::new("x")
            ],
            Some("t")
        )?,
        usage
    );
    Ok(())
}

#[test]
fn a_relative_program_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = doctor(
        &[&scene.path("plugins.toml"), Path::new("claude")],
        Some("t"),
    )?;

    assert_eq!(
        outcome,
        refused("the claude program claude is not an absolute path")
    );
    Ok(())
}

#[test]
fn a_missing_or_empty_token_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let args: [&Path; 2] = [&scene.path("plugins.toml"), &scene.program()];
    let unset = refused("CLAUDE_CODE_OAUTH_TOKEN is not set");

    assert_eq!(doctor(&args, None)?, unset);
    assert_eq!(doctor(&args, Some(""))?, unset);
    assert!(!scene.path("bin/argv").exists());
    Ok(())
}

#[test]
fn a_token_that_is_not_unicode_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let output = Command::new(env!("CARGO_BIN_EXE_assay"))
        .arg("doctor")
        .arg(scene.path("plugins.toml"))
        .arg(scene.program())
        .env("CLAUDE_CODE_OAUTH_TOKEN", OsStr::from_bytes(b"t\xff"))
        .output()?;

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "assay: CLAUDE_CODE_OAUTH_TOKEN is not valid Unicode\n"
    );
    assert!(!scene.path("bin/argv").exists());
    Ok(())
}

#[test]
fn a_config_that_is_not_utf8_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    fs::write(scene.path("plugins.toml"), b"[assay]\n# \xff\n")?;

    let outcome = doctor(&[&scene.path("plugins.toml"), &scene.program()], Some("t"))?;

    assert_eq!(
        outcome,
        refused(&format!(
            "{} is not UTF-8",
            scene.path("plugins.toml").display()
        ))
    );
    Ok(())
}

#[test]
fn an_unreadable_config_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let absent = scene.path("absent.toml");

    let outcome = doctor(&[&absent, &scene.program()], Some("t"))?;

    assert_eq!(outcome.code, Some(2));
    assert!(
        outcome
            .stderr
            .starts_with(&format!("assay: cannot read {}: ", absent.display())),
        "{}",
        outcome.stderr
    );
    Ok(())
}

#[test]
fn a_config_that_does_not_parse_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    fs::write(scene.path("plugins.toml"), "[assay]\n")?;

    let outcome = doctor(&[&scene.path("plugins.toml"), &scene.program()], Some("t"))?;

    assert_eq!(outcome.code, Some(2));
    assert!(
        outcome
            .stderr
            .starts_with("assay: plugins.toml is not a valid configuration"),
        "{}",
        outcome.stderr
    );
    Ok(())
}

#[test]
fn a_config_without_a_cli_pin_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    fs::write(
        scene.path("plugins.toml"),
        "[assay]\ndigest = \"sha256:dc7c1d8e3ccab68cfdb828ce40a446c3913f7172f899ffe3f439839cfd917a8b\"\n",
    )?;

    let outcome = doctor(&[&scene.path("plugins.toml"), &scene.program()], Some("t"))?;

    assert_eq!(
        outcome,
        refused("plugins.toml pins no claude_cli, so there is no CLI to check")
    );
    Ok(())
}

#[test]
fn a_config_at_the_read_cap_is_read_and_one_byte_more_is_refused() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;
    let pins = fs::read_to_string(scene.path("plugins.toml"))?;
    let cap = 1 << 20;
    let padding = cap - pins.len() - 2;
    let at_cap = format!("{pins}#{}\n", "x".repeat(padding));
    let over = format!("{at_cap} ");
    fs::write(scene.path("at-cap.toml"), &at_cap)?;
    fs::write(scene.path("over.toml"), &over)?;

    let read = doctor(&[&scene.path("at-cap.toml"), &scene.program()], Some("t"))?;
    let refused_over = doctor(&[&scene.path("over.toml"), &scene.program()], Some("t"))?;

    assert_eq!(at_cap.len(), cap);
    assert_eq!(read.code, Some(0), "{}", read.stderr);
    assert_eq!(
        refused_over,
        refused(&format!(
            "{} is larger than 1048576 bytes",
            scene.path("over.toml").display()
        ))
    );
    Ok(())
}

#[test]
fn a_cli_that_cannot_start_is_an_error() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = doctor(
        &[&scene.path("plugins.toml"), &scene.path("bin/absent")],
        Some("t"),
    )?;

    assert_eq!(outcome.code, Some(2));
    assert!(
        outcome
            .stderr
            .starts_with("assay: the canary could not be run"),
        "{}",
        outcome.stderr
    );
    Ok(())
}

#[test]
fn an_error_names_every_cause_down_to_the_os() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new("")?;

    let outcome = doctor(
        &[&scene.path("plugins.toml"), &scene.path("bin/absent")],
        Some("t"),
    )?;

    // doctor's error, the runner's spawn error, then the OS's.
    assert_eq!(
        outcome.stderr,
        "assay: the canary could not be run: the claude CLI could not be started: \
         No such file or directory (os error 2)\n"
    );
    Ok(())
}
