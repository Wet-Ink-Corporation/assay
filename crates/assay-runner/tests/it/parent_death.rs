//! The parent-death guard (#154): with a `ParentDeath` naming a `setpriv`
//! (`tests/fixtures/fake-setpriv`, which records its argv and execs the rest), every `claude`
//! the runner starts (model calls, status commands, both canaries) is started as
//! `setpriv --pdeathsig KILL -- <claude> <argv>`, with the claude argv and pid unchanged. The
//! stand-in is chosen through `ParentDeath::probe_paths`, so no test depends on the host's
//! `setpriv`; the real one is exercised end to end by `assay-cli`'s `tests/it/doctor.rs`.

use std::assert_matches;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{
    ArtifactNonce, CanaryProbe, CanaryToken, CliProbe, ModelOutcome, ModelRunner, ProfileProbe,
    ProfileWords,
};
use assay_runner::{
    ClaudeCanary, ClaudeHeadless, HeadlessEnv, HeadlessError, ParentDeath, Workdir,
};
use tempfile::TempDir;

use crate::support::{Asked, RecordingKill, call, fixtures_dir, stream_path};

const TOKEN: &str = "sk-ant-oat01-test-token";
const NONCE: &str = "0123456789abcdef0123456789abcdef";

/// The arguments `setpriv` is started with before the program.
const GUARD: [&str; 3] = ["--pdeathsig", "KILL", "--"];

/// A scratch directory with the canary stand-ins for `claude` and their knobs (`bin/`), an
/// empty working directory, a directory canary sandboxes are made in and one for the
/// stand-ins' records.
struct Scene {
    root: TempDir,
    killer: RecordingKill,
}

impl Scene {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        for dir in ["bin", "empty", "sandboxes", "record"] {
            fs::create_dir(root.path().join(dir))?;
        }
        let bin = root.path().join("bin");
        fs::copy(
            fixtures_dir().join("fake-claude-canary"),
            bin.join("claude-canary"),
        )?;
        fs::copy(
            fixtures_dir().join("fake-claude-profile"),
            bin.join("claude-profile"),
        )?;
        let utf8 = |path: PathBuf| path.to_str().map(str::to_owned).ok_or("not UTF-8");
        fs::write(
            bin.join("knobs"),
            format!(
                "STREAM='{}'\nVERSION='{}'\nAUTH='{}'\n",
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

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The environment of every stand-in: the token and the record directory.
    fn env(&self) -> Result<HeadlessEnv, Box<dyn Error>> {
        let mut env = HeadlessEnv::new(TOKEN);
        env.pass("FAKE_RECORD", self.path("record"))?;
        Ok(env)
    }

    /// A runner of `fake-claude` under the stand-in `setpriv`, printing the stream `name` and
    /// hanging after it when `hang`.
    fn runner(
        &self,
        name: &str,
        hang: bool,
    ) -> Result<ClaudeHeadless<&RecordingKill>, Box<dyn Error>> {
        Ok(self.direct(name, hang)?.with_parent_death(stand_in()?))
    }

    /// The same runner, started directly.
    fn direct(
        &self,
        name: &str,
        hang: bool,
    ) -> Result<ClaudeHeadless<&RecordingKill>, Box<dyn Error>> {
        let mut env = self.env()?;
        env.pass("FAKE_STREAM", stream_path(name))?;
        env.pass(
            "FAKE_VERSION",
            fixtures_dir().join("status/version_2_1_296.txt"),
        )?;
        env.pass(
            "FAKE_AUTH",
            fixtures_dir().join("status/auth_oauth_2_1_296.json"),
        )?;
        if hang {
            env.pass("FAKE_HANG", "1")?;
        }
        Ok(ClaudeHeadless::new(
            fixtures_dir().join("fake-claude"),
            Workdir::new(self.path("empty"))?,
            env,
            &self.killer,
        ))
    }

    fn canary(&self, program: &str) -> Result<ClaudeCanary<&RecordingKill>, Box<dyn Error>> {
        Ok(
            ClaudeCanary::new(self.path("bin").join(program), self.env()?, &self.killer)
                .in_dir(self.path("sandboxes"))
                .with_parent_death(stand_in()?),
        )
    }

    /// Each argv the stand-in `setpriv` was started with, in order.
    fn setpriv_starts(&self) -> Result<Vec<Vec<String>>, Box<dyn Error>> {
        let raw = fs::read_to_string(self.path("record/setpriv"))?;
        Ok(raw
            .lines()
            .map(|line| line.split_terminator('\0').map(str::to_owned).collect())
            .collect())
    }

    /// The argv `fake-claude` recorded as `name`.
    fn claude_argv(&self, name: &str) -> Result<Vec<String>, Box<dyn Error>> {
        let raw = fs::read_to_string(self.path("record").join(name))?;
        Ok(raw.split_terminator('\0').map(str::to_owned).collect())
    }
}

/// The guard naming the stand-in `setpriv`, as the probe finds it. The fixture is used in
/// place, never copied: a copy written by this process could be held open by a sibling test's
/// fork when it is executed (ETXTBSY, #176).
fn stand_in() -> Result<ParentDeath, Box<dyn Error>> {
    guard_at(&fixtures_dir().join("fake-setpriv"))
}

/// The guard the probe finds for `setpriv` alone.
fn guard_at(setpriv: &Path) -> Result<ParentDeath, Box<dyn Error>> {
    let guard = ParentDeath::probe_paths([setpriv]);
    if guard.setpriv() == Some(setpriv) {
        Ok(guard)
    } else {
        Err(format!("{} was not probed: {guard:?}", setpriv.display()).into())
    }
}

/// `GUARD`, `program`, then `args`: the argv `setpriv` is started with.
fn guarded(program: &Path, args: &[String]) -> Result<Vec<String>, Box<dyn Error>> {
    let program = program.to_str().ok_or("program is not UTF-8")?.to_owned();
    Ok(GUARD
        .map(str::to_owned)
        .into_iter()
        .chain([program])
        .chain(args.iter().cloned())
        .collect())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-ins; Miri cannot run processes")]
fn a_model_call_is_started_under_setpriv_with_its_argv_unchanged() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let unguarded = Scene::new()?;
    unguarded.direct("ok", false)?.run(&call()?)?;

    let result = scene.runner("ok", false)?.run(&call()?)?;

    let argv = scene.claude_argv("argv")?;
    assert_eq!(argv, unguarded.claude_argv("argv")?, "the 03 §8 argv");
    assert_eq!(
        scene.setpriv_starts()?,
        [guarded(&fixtures_dir().join("fake-claude"), &argv)?]
    );
    assert_matches!(result.outcome, ModelOutcome::Ok(_));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-ins; Miri cannot run processes")]
fn under_setpriv_the_group_killed_at_the_timeout_is_claudes_own_pid() -> Result<(), Box<dyn Error>>
{
    let scene = Scene::new()?;
    let mut call = call()?;
    call.timeout = Duration::from_millis(300);

    let result = scene.runner("ok", true)?.run(&call)?;

    // setpriv execs claude, so the child assay started, whose pid is the group id, is claude.
    let pid: u32 = fs::read_to_string(scene.path("record/pid"))?
        .trim()
        .parse()?;
    assert_eq!(
        scene.killer.take_asked(),
        [Asked::Led {
            group: pid,
            leader_unreaped: true
        }]
    );
    assert_eq!(result.outcome, ModelOutcome::Timeout);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-ins; Miri cannot run processes")]
fn the_status_commands_are_started_under_setpriv() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;

    scene.runner("ok", false)?.status()?;

    let program = fixtures_dir().join("fake-claude");
    assert_eq!(
        scene.setpriv_starts()?,
        [
            guarded(&program, &["--version".to_owned()])?,
            guarded(&program, &["auth".to_owned(), "status".to_owned()])?,
        ]
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-ins; Miri cannot run processes")]
fn the_mod_canary_is_started_under_setpriv() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let token = CanaryToken::new(&NONCE.parse::<ArtifactNonce>()?);

    scene.canary("claude-canary")?.probe(&token)?;

    let argv = fs::read_to_string(scene.path("bin/argv"))?;
    let argv: Vec<String> = argv.split_terminator('\0').map(str::to_owned).collect();
    assert_eq!(
        scene.setpriv_starts()?,
        [guarded(&scene.path("bin/claude-canary"), &argv)?]
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns the stand-ins; Miri cannot run processes")]
fn the_profile_canary_is_started_under_setpriv() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let nonce: ArtifactNonce = NONCE.parse()?;
    let words = ProfileWords::new([&nonce, &nonce, &nonce, &nonce]);

    scene.canary("claude-profile")?.probe_profile(&words)?;

    let starts = scene.setpriv_starts()?;
    let [start] = starts.as_slice() else {
        return Err(format!("one start expected, got {starts:?}").into());
    };
    let (head, claude_args) = start.split_at_checked(4).ok_or("argv too short")?;
    let program = scene.path("bin/claude-profile");
    assert_eq!(head, guarded(&program, &[])?);
    assert_eq!(claude_args.first().map(String::as_str), Some("-p"));
    assert_eq!(
        claude_args.last().map(String::as_str),
        Some("--include-hook-events")
    );
    Ok(())
}

/// `setpriv` would start, fail to exec the CLI and exit, leaving an empty transcript charged at
/// the call's budget; the program is checked first, so this stays the spawn error it is without
/// `setpriv`, and nothing is charged.
#[test]
#[cfg_attr(miri, ignore = "tries to spawn a process; Miri cannot")]
fn a_cli_that_cannot_start_under_setpriv_is_a_spawn_error_that_never_ran()
-> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    let not_executable = scene.path("bin/knobs");
    for program in [scene.path("bin/absent"), not_executable, scene.path("bin")] {
        let runner = ClaudeHeadless::new(
            &program,
            Workdir::new(scene.path("empty"))?,
            scene.env()?,
            &scene.killer,
        )
        .with_parent_death(stand_in()?);

        let ran = runner.run(&call()?);

        let error = ran.err().ok_or("a CLI that cannot start ran")?;
        assert_matches!(error, HeadlessError::Spawn(_), "{}", program.display());
        assert!(!error.may_have_run());
    }
    assert!(
        !scene.path("record/setpriv").exists(),
        "setpriv was never started"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "tries to spawn a process; Miri cannot")]
fn a_setpriv_gone_after_the_probe_is_a_spawn_error_that_never_ran() -> Result<(), Box<dyn Error>> {
    let scene = Scene::new()?;
    // A copy that is never executed, so no ETXTBSY concern.
    let setpriv = scene.path("bin/setpriv");
    fs::copy(fixtures_dir().join("fake-setpriv"), &setpriv)?;
    let runner = scene
        .direct("ok", false)?
        .with_parent_death(guard_at(&setpriv)?);
    fs::remove_file(&setpriv)?;

    let ran = runner.run(&call()?);

    let error = ran.err().ok_or("the call ran without its setpriv")?;
    assert_matches!(error, HeadlessError::Spawn(_));
    assert!(!error.may_have_run());
    assert!(!scene.path("record/argv").exists(), "claude never started");
    Ok(())
}
