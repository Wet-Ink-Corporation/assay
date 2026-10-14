//! `ForgeSupervisor`: `assay-forge`'s process-group supervisor lent to the gate adapters
//! (`assay_gates::Supervisor`) and to `ClaudeHeadless` (`assay_runner::GroupKill`), wi-603c63.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{ModelCall, ModelRunner, Prompt, Termination};
use assay_cli::ForgeSupervisor;
use assay_domain::Aeu;
use assay_forge::process::{GroupKiller, ProcessError};
use assay_gates::{CaptureSupervisor, CapturedRun, ProcessSpec, Supervisor};
use assay_runner::{ClaudeHeadless, HeadlessEnv, Workdir};
use tempfile::TempDir;

use crate::support::write_executable;

fn supervisor() -> Result<ForgeSupervisor, Box<dyn Error>> {
    Ok(ForgeSupervisor::new(GroupKiller::system()?))
}

/// A gate played by `sh -c script`, with `$0` set to `arg`.
struct Shell<'a> {
    script: &'a str,
    arg: &'a Path,
    cwd: &'a Path,
    env: &'a [(OsString, OsString)],
    timeout: Duration,
}

impl<'a> Shell<'a> {
    /// `script` run in `dir` (also its `$0`) with an empty environment and a generous timeout.
    fn in_dir(script: &'a str, dir: &'a Path) -> Self {
        Self {
            script,
            arg: dir,
            cwd: dir,
            env: &[],
            timeout: GENEROUS,
        }
    }

    /// Runs the shell through the supervisor with a 1 KiB output cap.
    fn run(&self) -> Result<Result<Termination, ProcessError>, Box<dyn Error>> {
        let args = [
            "-c".to_owned(),
            self.script.to_owned(),
            self.arg.to_string_lossy().into_owned(),
        ];
        let spec = ProcessSpec {
            program: Path::new("/bin/sh"),
            args: &args,
            cwd: self.cwd,
            env: self.env,
            timeout: self.timeout,
            max_output_bytes: 1024,
        };
        Ok(supervisor()?.run(&spec))
    }
}

/// Whether the process `pid` is gone or a zombie (holds no files, runs nothing).
fn is_dead(pid: &str) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_owned))
        })
        .is_none_or(|state| state == "Z")
}

const GENEROUS: Duration = Duration::from_secs(30);

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_gate_that_exits_reports_its_exit_code() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;

    let ended = Shell::in_dir("exit 3", scratch.path()).run()?;

    assert_eq!(ended?, Termination::Exited(3));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_gate_ended_by_a_signal_is_signalled() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;

    let ended = Shell::in_dir("kill -9 $$", scratch.path()).run()?;

    assert_eq!(ended?, Termination::Signalled);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_gate_past_its_deadline_times_out_and_its_whole_group_is_gone() -> Result<(), Box<dyn Error>> {
    // The shell starts a background `sleep`, records its pid and exits; its exit is the
    // readiness signal, so the grandchild exists before the supervisor starts waiting on it.
    let scratch = tempfile::tempdir()?;
    let pid_file = scratch.path().join("grandchild.pid");
    let env = [(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))];
    let shell = Shell {
        arg: &pid_file,
        env: &env,
        timeout: Duration::from_secs(2),
        ..Shell::in_dir(r#"sleep 600 & echo $! > "$0"; exit 0"#, scratch.path())
    };

    let ended = shell.run()?;

    let grandchild = fs::read_to_string(&pid_file)?;
    assert_eq!(ended?, Termination::TimedOut);
    assert!(
        is_dead(grandchild.trim()),
        "the grandchild must be killed with its group"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_gate_past_its_output_cap_is_output_too_large() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;

    let ended = Shell::in_dir("while :; do echo y; done", scratch.path()).run()?;

    assert_eq!(ended?, Termination::OutputTooLarge);
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_gate_runs_in_its_directory_with_exactly_its_environment() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let record = scratch.path().join("record");
    let cwd = scratch.path().join("checkout");
    fs::create_dir(&cwd)?;
    let env = [(OsString::from("ONLY"), OsString::from("this"))];
    let shell = Shell {
        arg: &record,
        env: &env,
        ..Shell::in_dir(r#"pwd -P > "$0.cwd"; /usr/bin/env > "$0.env""#, &cwd)
    };

    shell.run()??;

    let names: BTreeSet<String> = fs::read_to_string(record.with_extension("env"))?
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
        // The shell adds these itself.
        .filter(|name| !["PWD", "SHLVL", "_"].contains(&name.as_str()))
        .collect();
    assert_eq!(names, BTreeSet::from(["ONLY".to_owned()]));
    assert_eq!(
        fs::read_to_string(record.with_extension("cwd"))?,
        format!("{}\n", fs::canonicalize(&cwd)?.display())
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_bare_program_is_found_on_the_gates_own_path() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let env = [(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))];
    let spec = ProcessSpec {
        program: Path::new("true"),
        args: &[],
        cwd: scratch.path(),
        env: &env,
        timeout: GENEROUS,
        max_output_bytes: 1024,
    };

    let ended = supervisor()?.run(&spec);

    assert_eq!(ended?, Termination::Exited(0));
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_captured_run_keeps_stdout_and_its_exit_code() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let args = [
        "-c".to_owned(),
        "echo '[]'; echo noise >&2; exit 1".to_owned(),
    ];
    let spec = ProcessSpec {
        program: Path::new("/bin/sh"),
        args: &args,
        cwd: scratch.path(),
        env: &[],
        timeout: GENEROUS,
        max_output_bytes: 1024,
    };

    let run = supervisor()?.capture(&spec)?;

    assert_eq!(
        run,
        CapturedRun {
            termination: Termination::Exited(1),
            stdout: b"[]\n".to_vec(),
        },
        "stdout is kept, stderr is not"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_captured_run_past_its_output_cap_keeps_nothing() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let args = ["-c".to_owned(), "while :; do echo y; done".to_owned()];
    let spec = ProcessSpec {
        program: Path::new("/bin/sh"),
        args: &args,
        cwd: scratch.path(),
        env: &[],
        timeout: GENEROUS,
        max_output_bytes: 1024,
    };

    let run = supervisor()?.capture(&spec)?;

    assert_eq!(
        run,
        CapturedRun {
            termination: Termination::OutputTooLarge,
            stdout: Vec::new(),
        },
        "a truncated output is never handed on"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "tries to spawn a process; Miri cannot")]
fn a_gate_that_cannot_start_is_a_supervisor_error() -> Result<(), Box<dyn Error>> {
    let scratch = tempfile::tempdir()?;
    let spec = ProcessSpec {
        program: Path::new("/nonexistent/assay-gate"),
        args: &[],
        cwd: scratch.path(),
        env: &[],
        timeout: GENEROUS,
        max_output_bytes: 1024,
    };

    let ended = supervisor()?.run(&spec);

    assert!(
        matches!(ended, Err(ProcessError::Spawn(_))),
        "a missing program is a spawn failure, got {ended:?}"
    );
    Ok(())
}

/// A stand-in `claude` that starts a helper in its group, records the helper's pid at
/// `$HELPER_PID`, reads its prompt and then hangs or exits.
struct FakeClaude {
    root: TempDir,
}

impl FakeClaude {
    fn new(then: &str) -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let script = root.path().join("claude");
        write_executable(
            &script,
            &format!(
                "#!/bin/sh\n/bin/sleep 600 &\necho $! > \"$HELPER_PID\"\n/bin/cat > /dev/null\n{then}\n"
            ),
        )?;
        fs::create_dir(root.path().join("empty"))?;
        Ok(Self { root })
    }

    fn helper_pid_file(&self) -> PathBuf {
        self.root.path().join("helper.pid")
    }

    fn runner(&self) -> Result<ClaudeHeadless<ForgeSupervisor>, Box<dyn Error>> {
        let mut env = HeadlessEnv::new("sk-ant-oat01-test-token");
        env.pass("HELPER_PID", self.helper_pid_file())?;
        Ok(ClaudeHeadless::new(
            self.root.path().join("claude"),
            Workdir::new(self.root.path().join("empty"))?,
            env,
            supervisor()?,
        ))
    }

    fn helper_is_dead(&self) -> Result<bool, Box<dyn Error>> {
        Ok(is_dead(fs::read_to_string(self.helper_pid_file())?.trim()))
    }
}

fn call(timeout: Duration) -> Result<ModelCall, Box<dyn Error>> {
    Ok(ModelCall {
        system_prompt_file: PathBuf::from("/bundle/system.md"),
        user_prompt: Prompt::new("Context pack, then the role instruction.")?,
        json_schema: r#"{"type":"object"}"#.to_owned(),
        add_dir: PathBuf::from("/work/pr"),
        max_turns: NonZeroU8::new(8).ok_or("8 is not zero")?,
        budget: Aeu::from_micros(740_000),
        timeout,
    })
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn claude_and_every_helper_it_started_are_gone_after_a_timeout() -> Result<(), Box<dyn Error>> {
    let claude = FakeClaude::new("exec /bin/sleep 600")?;

    claude.runner()?.run(&call(Duration::from_secs(2))?)?;

    assert!(
        claude.helper_is_dead()?,
        "the helper must die with claude's group"
    );
    Ok(())
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
fn a_helper_left_running_by_claude_does_not_outlive_the_call() -> Result<(), Box<dyn Error>> {
    let claude = FakeClaude::new("exit 0")?;

    claude.runner()?.run(&call(GENEROUS)?)?;

    assert!(
        claude.helper_is_dead()?,
        "the helper must not outlive the call"
    );
    Ok(())
}
