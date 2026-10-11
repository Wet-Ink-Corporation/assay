//! Bounded child processes: a wall-clock deadline, capped output, a process group that is
//! killed rather than left behind, and a child that is always waited on (H-13).
//!
//! stdin, stdout and stderr are anonymous temporary files, not pipes, so this process never
//! blocks on a descendant: there is no reader that a grandchild holding a pipe open could
//! keep waiting, and so no join that could outlast the deadline. The calling thread polls the
//! child, the size of its output and, once the child has exited, the rest of its process
//! group (through Linux `/proc`) against one deadline. A run is complete only when the child
//! has exited and no live process is left in its group.
//!
//! On a timeout or an over-cap output the whole group is killed with the system `kill`
//! ([`GroupKiller`]), because std cannot signal a group without `unsafe`. Its exit status is
//! not believed; the kill counts only once `/proc` shows no live member of the group, and
//! anything else is [`ProcessError::Kill`]. The child itself is then killed through std and
//! always waited on. A group is never signalled once its leader has been reaped and no member
//! is left, because the group id may then be reused; the remaining window is a member exiting
//! between the `/proc` scan and the signal (microseconds), after which an unrelated process
//! would have to take that id as its own group id before the signal lands.
//!
//! Runtime prerequisites: Linux `/proc` and a procps or util-linux `kill` in `/usr/bin` or
//! `/bin`. [`GroupKiller::system`] checks the latter up front, so a host without it fails
//! closed before any process starts.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running child, its output and its group are polled. Short against any timeout
/// we configure, long enough that polling costs nothing measurable.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Default time the kill program has to run, and the group then has to be gone.
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(5);

/// Where [`GroupKiller::system`] looks for `kill`, in order.
const SYSTEM_KILL: [&str; 2] = ["/usr/bin/kill", "/bin/kill"];

/// `PATH` for the kill program; fixed rather than inherited.
const SYSTEM_PATH: &str = "/usr/bin:/bin";

/// `errno` for "no such process", which reading `/proc/<pid>/stat` of a process that has
/// just exited can return.
const ESRCH: i32 = 3;

/// Bounds for one child process.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Wall-clock time from spawn until the child's group is killed.
    pub(crate) timeout: Duration,
    /// Most bytes accepted on stdout, and separately on stderr.
    pub(crate) max_output_bytes: usize,
}

/// What a finished child produced.
#[derive(Debug)]
pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Why a child process produced no usable output.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The timeout does not fit in the clock.
    #[error("the timeout is too long to represent")]
    TimeoutTooLong,
    /// The temporary files for the child's stdin, stdout or stderr could not be used.
    #[error("could not use a temporary file for the process's input or output")]
    Spool(#[source] io::Error),
    /// The program could not be started.
    #[error("could not start the process")]
    Spawn(#[source] io::Error),
    /// The child, or a process it started, was still running at the deadline and was killed.
    #[error("the process did not finish before its deadline")]
    TimedOut,
    /// The child wrote more than the cap on stdout or stderr and was killed.
    #[error("the process wrote more output than allowed")]
    OutputTooLarge,
    /// Polling the child or reading `/proc` failed.
    #[error("could not observe the process")]
    Io(#[source] io::Error),
    /// The process group had to be killed after `cause`, and it could not be shown to be
    /// gone. The child itself was still killed and waited on.
    #[error("could not kill the process group after: {cause}")]
    Kill {
        /// The failure that made the kill necessary.
        cause: Box<ProcessError>,
        /// Why the group kill failed.
        #[source]
        source: io::Error,
    },
}

/// Why no usable `kill` program was found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KillerError {
    /// None of the system locations holds an executable `kill`.
    #[error(
        "no executable kill in {searched:?}: a procps or util-linux kill is a runtime prerequisite"
    )]
    NotFound {
        /// The locations searched.
        searched: &'static [&'static str],
    },
    /// The given program is missing or is not an executable file.
    #[error("{} is not an executable file", path.display())]
    NotExecutable {
        /// The program that was refused.
        path: PathBuf,
    },
}

/// The program that kills a timed-out child's whole process group: a procps or util-linux
/// compatible `kill` taking `-s KILL -- -<pgid>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupKiller {
    program: PathBuf,
    grace: Duration,
}

impl GroupKiller {
    /// The system `kill` from `/usr/bin` or `/bin`, with a five-second grace.
    ///
    /// # Errors
    /// [`KillerError::NotFound`] when neither location holds an executable `kill`.
    pub fn system() -> Result<Self, KillerError> {
        SYSTEM_KILL
            .into_iter()
            .find_map(|program| Self::at(program, KILL_GRACE).ok())
            .ok_or(KillerError::NotFound {
                searched: &SYSTEM_KILL,
            })
    }

    /// `program` as the kill program; it, and then the group's exit, get `grace` each.
    ///
    /// # Errors
    /// [`KillerError::NotExecutable`] when `program` is not an executable file.
    pub fn at(program: impl Into<PathBuf>, grace: Duration) -> Result<Self, KillerError> {
        let program = program.into();
        let executable = fs::metadata(&program)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
        if executable {
            Ok(Self { program, grace })
        } else {
            Err(KillerError::NotExecutable { path: program })
        }
    }

    /// Sends SIGKILL to process group `group` and waits, within the grace, until no live
    /// member is left. The program's exit status is not believed either way.
    fn kill_group(&self, group: u32) -> io::Result<()> {
        let deadline = Instant::now()
            .checked_add(self.grace)
            .ok_or_else(|| io::Error::other("the kill grace overflows the clock"))?;
        let mut kill = Command::new(&self.program);
        kill.env_clear()
            .env("PATH", SYSTEM_PATH)
            .args(["-s", "KILL", "--"])
            .arg(format!("-{group}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut killer = kill.spawn()?;
        if poll_until(deadline, || killer.try_wait())?.is_none() {
            let stopped = killer.kill();
            let reaped = killer.wait().map(drop);
            stopped.and(reaped)?;
        }
        let gone = poll_until(deadline, || Ok((!group_alive(group)?).then_some(())))?;
        gone.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "the process group still has live members after the kill",
            )
        })
    }
}

/// Runs `command` with `input` on stdin, within `limits`, and returns its output.
///
/// The child leads a new process group. On a timeout or an over-cap output the group is
/// killed with `killer`; the child is always waited on.
pub(crate) fn run(
    mut command: Command,
    input: &[u8],
    limits: Limits,
    killer: &GroupKiller,
) -> Result<Captured, ProcessError> {
    let deadline = Instant::now()
        .checked_add(limits.timeout)
        .ok_or(ProcessError::TimeoutTooLong)?;
    let cap = limits.max_output_bytes;
    let stdout = tempfile::tempfile().map_err(ProcessError::Spool)?;
    let stderr = tempfile::tempfile().map_err(ProcessError::Spool)?;
    command
        .stdin(stdin_from(input)?)
        .stdout(child_end(&stdout)?)
        .stderr(child_end(&stderr)?)
        .process_group(0);
    let mut child = command.spawn().map_err(ProcessError::Spawn)?;
    // Closes this process's copies of the child's ends.
    drop(command);

    let outcome = supervise(&mut child, [&stdout, &stderr], cap, deadline)
        .map_err(|interrupted| terminate(&mut child, interrupted, killer));
    let reaped = child.wait();
    let status = match (outcome, reaped) {
        (Ok(status), Ok(_)) => status,
        (Ok(_), Err(source)) => return Err(ProcessError::Io(source)),
        // After a kill, `wait` can only fail with ECHILD (already reaped); the cause is what
        // the caller must see, so a timeout stays a timeout.
        (Err(cause), _) => return Err(cause),
    };
    Ok(Captured {
        status,
        stdout: read_capped(stdout, cap)?,
        stderr: read_capped(stderr, cap)?,
    })
}

/// stdin for the child: closed when `input` is empty, else a temporary file holding it.
fn stdin_from(input: &[u8]) -> Result<Stdio, ProcessError> {
    if input.is_empty() {
        return Ok(Stdio::null());
    }
    let mut file = tempfile::tempfile().map_err(ProcessError::Spool)?;
    file.write_all(input).map_err(ProcessError::Spool)?;
    file.seek(SeekFrom::Start(0)).map_err(ProcessError::Spool)?;
    Ok(Stdio::from(file))
}

/// A second handle on an output file for the child to write through.
fn child_end(file: &File) -> Result<Stdio, ProcessError> {
    file.try_clone()
        .map(Stdio::from)
        .map_err(ProcessError::Spool)
}

/// Why supervision stopped before the run completed.
#[derive(Debug)]
struct Interrupted {
    cause: ProcessError,
    /// The child had exited and was reaped by `try_wait`.
    leader_reaped: bool,
}

/// Polls until the child has exited and its group is empty, the output passes `cap`, or
/// `deadline` passes.
fn supervise(
    child: &mut Child,
    outputs: [&File; 2],
    cap: usize,
    deadline: Instant,
) -> Result<ExitStatus, Interrupted> {
    let group = child.id();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX);
    let mut exited = None;
    let polled = poll_until(deadline, || {
        for output in outputs {
            if output.metadata()?.len() > limit {
                return Ok(Some(Err(ProcessError::OutputTooLarge)));
            }
        }
        if exited.is_none() {
            exited = child.try_wait()?;
        }
        match exited {
            Some(status) if !group_alive(group)? => Ok(Some(Ok(status))),
            _ => Ok(None),
        }
    });
    let leader_reaped = exited.is_some();
    let cause = match polled {
        Ok(Some(Ok(status))) => return Ok(status),
        Ok(Some(Err(cause))) => cause,
        Ok(None) => ProcessError::TimedOut,
        Err(source) => ProcessError::Io(source),
    };
    Err(Interrupted {
        cause,
        leader_reaped,
    })
}

/// Kills what is left of the child's group after `interrupted`, and the child itself.
fn terminate(child: &mut Child, interrupted: Interrupted, killer: &GroupKiller) -> ProcessError {
    let Interrupted {
        cause,
        leader_reaped,
    } = interrupted;
    let group = child.id();
    // A reaped leader with no live member left: nothing to kill, and the id may be reused.
    if leader_reaped && matches!(group_alive(group), Ok(false)) {
        return cause;
    }
    let group_killed = killer.kill_group(group);
    // Already-exited children report `Ok` here.
    let leader_killed = child.kill();
    match group_killed.and(leader_killed) {
        Ok(()) => cause,
        Err(source) => ProcessError::Kill {
            cause: Box::new(cause),
            source,
        },
    }
}

/// Reads a whole output file, or fails when it holds more than `cap` bytes.
fn read_capped(mut file: File, cap: usize) -> Result<Vec<u8>, ProcessError> {
    file.seek(SeekFrom::Start(0)).map_err(ProcessError::Spool)?;
    let limit = u64::try_from(cap).map_or(u64::MAX, |cap| cap.saturating_add(1));
    let mut buffer = Vec::new();
    file.take(limit)
        .read_to_end(&mut buffer)
        .map_err(ProcessError::Spool)?;
    if buffer.len() > cap {
        return Err(ProcessError::OutputTooLarge);
    }
    Ok(buffer)
}

/// Whether any live (not zombie) process is in process group `group`, from `/proc`.
fn group_alive(group: u32) -> io::Result<bool> {
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let is_pid = entry
            .file_name()
            .to_str()
            .is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()));
        if !is_pid {
            continue;
        }
        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            // The process exited between the listing and the read.
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(ESRCH) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let member = in_group(&stat, group).map_err(|UnreadableStat| {
            io::Error::new(io::ErrorKind::InvalidData, "unreadable /proc/<pid>/stat")
        })?;
        if member {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A `/proc/<pid>/stat` line that does not have the documented shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnreadableStat;

/// Whether a `/proc/<pid>/stat` line (`pid (comm) state ppid pgrp ...`) is a live member of
/// `group`. `comm` may contain `) `, so the fields start after the last one.
fn in_group(stat: &str, group: u32) -> Result<bool, UnreadableStat> {
    let (_, fields) = stat.rsplit_once(") ").ok_or(UnreadableStat)?;
    let mut fields = fields.split_whitespace();
    let state = fields.next().ok_or(UnreadableStat)?;
    let pgrp: u32 = fields
        .nth(1)
        .and_then(|pgrp| pgrp.parse().ok())
        .ok_or(UnreadableStat)?;
    Ok(pgrp == group && state != "Z" && state != "X")
}

/// Calls `probe` until it yields `Some` or `deadline` passes (`None`). The deadline is checked
/// first, so a zero timeout never probes.
fn poll_until<T>(
    deadline: Instant,
    mut probe: impl FnMut() -> io::Result<Option<T>>,
) -> io::Result<Option<T>> {
    loop {
        let Some(left) = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
        else {
            return Ok(None);
        };
        if let Some(found) = probe()? {
            return Ok(Some(found));
        }
        // std has no wait-with-timeout on `Child`, and this is synchronous code, so a short
        // bounded sleep between polls is the deadline mechanism (not a test sleep, TST-10).
        thread::sleep(left.min(POLL_INTERVAL));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const GENEROUS: Limits = Limits {
        timeout: Duration::from_secs(30),
        max_output_bytes: 1024,
    };

    fn system_killer() -> GroupKiller {
        GroupKiller::system().expect("procps or util-linux kill is installed")
    }

    /// An executable shell script at `dir/name` with `body`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make it executable");
        path
    }

    /// Whether `pid` is gone or a zombie (holds no files, runs nothing).
    fn is_dead(pid: &str) -> bool {
        let state = fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_owned))
            });
        state.as_deref().is_none_or(|state| state == "Z")
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn captures_stdout_and_exit_status() {
        let mut echo = Command::new("echo");
        echo.arg("hello");

        let output = run(echo, b"", GENEROUS, &system_killer()).expect("echo runs");

        assert_eq!(output.stdout, b"hello\n", "stdout must be captured");
        assert!(output.status.success(), "echo exits 0");
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn feeds_input_to_stdin() {
        let output =
            run(Command::new("cat"), b"paths\0", GENEROUS, &system_killer()).expect("cat runs");

        assert_eq!(output.stdout, b"paths\0", "cat echoes its stdin");
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn kills_and_reaps_a_child_that_outlives_the_deadline() {
        let mut sleeper = Command::new("sleep");
        sleeper.arg("30");
        let limits = Limits {
            timeout: Duration::from_millis(100),
            ..GENEROUS
        };

        let result = run(sleeper, b"", limits, &system_killer());

        assert!(
            matches!(result, Err(ProcessError::TimedOut)),
            "a child past its deadline is a timeout, got {result:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn kills_a_grandchild_that_outlives_the_child_past_the_deadline() {
        // The shell starts a background `sleep`, records its pid and exits; its exit is the
        // readiness signal, so the grandchild exists before the runner starts waiting on it.
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let pid_file = scratch.path().join("grandchild.pid");
        let mut shell = Command::new("sh");
        shell
            .arg("-c")
            .arg(r#"sleep 600 & echo $! > "$0"; exit 0"#)
            .arg(&pid_file);
        let limits = Limits {
            timeout: Duration::from_secs(2),
            ..GENEROUS
        };

        let result = run(shell, b"", limits, &system_killer());

        assert!(
            matches!(result, Err(ProcessError::TimedOut)),
            "a process group still running at the deadline is a timeout, got {result:?}"
        );
        let pid = fs::read_to_string(&pid_file).expect("the shell started the grandchild first");
        // `run` returns only once the group is verifiably gone; re-poll briefly regardless.
        let dead = (0..1000).any(|_| {
            thread::yield_now();
            is_dead(pid.trim())
        });
        assert!(
            dead,
            "the grandchild must have been killed with its process group"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn rejects_output_over_the_cap() {
        let mut yes = Command::new("yes");
        yes.arg("y");

        let result = run(yes, b"", GENEROUS, &system_killer());

        assert!(
            matches!(result, Err(ProcessError::OutputTooLarge)),
            "unbounded output must be cut off, got {result:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn reports_a_program_that_cannot_start() {
        let result = run(
            Command::new("/nonexistent/assay-test-binary"),
            b"",
            GENEROUS,
            &system_killer(),
        );

        assert!(
            matches!(result, Err(ProcessError::Spawn(_))),
            "a missing binary is a spawn failure, got {result:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn a_kill_that_leaves_the_group_alive_is_an_error_and_the_child_is_still_reaped() {
        // `true` exits 0 without signalling anything: the exit status must not be believed.
        let killer = GroupKiller::at("/bin/true", Duration::from_millis(100)).expect("executable");
        let mut sleeper = Command::new("sleep");
        sleeper.arg("30");
        let limits = Limits {
            timeout: Duration::from_millis(100),
            ..GENEROUS
        };

        let result = run(sleeper, b"", limits, &killer);

        assert!(
            matches!(
                &result,
                Err(ProcessError::Kill { cause, .. }) if matches!(**cause, ProcessError::TimedOut)
            ),
            "a group that survives the kill must surface, with the timeout as its cause, got {result:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn a_child_that_exits_cleanly_is_never_group_killed() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let marker = scratch.path().join("killed");
        let spy = script(
            scratch.path(),
            "kill",
            &format!("touch '{}'", marker.display()),
        );
        let killer = GroupKiller::at(&spy, Duration::from_millis(100)).expect("executable");
        let mut echo = Command::new("echo");
        echo.arg("done");

        let output = run(echo, b"", GENEROUS, &killer).expect("echo runs");

        assert_eq!(output.stdout, b"done\n", "the output is complete");
        assert!(
            !marker.exists(),
            "a reaped leader with an empty group must not be signalled (pgid reuse)"
        );
    }

    #[test]
    fn finds_the_system_kill() {
        assert!(
            GroupKiller::system().is_ok(),
            "the runtime prerequisite is present on this machine"
        );
    }

    #[test]
    fn rejects_a_kill_program_that_is_missing_or_not_executable() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let plain = scratch.path().join("kill");
        fs::write(&plain, "not a program").expect("write the file");

        assert!(
            matches!(
                GroupKiller::at(&plain, KILL_GRACE),
                Err(KillerError::NotExecutable { .. })
            ),
            "a file without execute permission is not a kill program"
        );
        assert!(
            matches!(
                GroupKiller::at(scratch.path().join("absent"), KILL_GRACE),
                Err(KillerError::NotExecutable { .. })
            ),
            "a missing file is not a kill program"
        );
    }

    #[test]
    fn a_process_in_the_group_is_alive_unless_it_is_a_zombie() {
        assert_eq!(
            in_group("42 (sh) S 1 42 42 0", 42),
            Ok(true),
            "a sleeping member"
        );
        assert_eq!(
            in_group("43 (odd) name) R 1 42 42 0", 42),
            Ok(true),
            "a command name containing `) ` is skipped over"
        );
        assert_eq!(in_group("44 (sh) Z 1 42 42 0", 42), Ok(false), "a zombie");
        assert_eq!(
            in_group("45 (sh) S 1 7 7 0", 42),
            Ok(false),
            "another group"
        );
        assert_eq!(
            in_group("garbage", 42),
            Err(UnreadableStat),
            "an unreadable stat line fails closed"
        );
    }
}
