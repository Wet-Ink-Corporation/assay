//! Bounded child processes: a wall-clock deadline, capped output, and a child that is always
//! waited on (H-13).
//!
//! stdout and stderr are drained by scoped threads while the calling thread polls the child,
//! and then those threads, against one deadline. The child leads its own process group, and
//! every failure kills the whole group before the threads are joined, so a grandchild holding
//! a pipe cannot stretch the deadline. Every path that leaves [`run`] after a successful spawn
//! has waited on the child.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

/// How often a running child is polled for exit. Short against any timeout we configure, long
/// enough that polling costs nothing measurable.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// How long the system `kill` may take to signal a process group.
const KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Where `kill` is looked up; fixed rather than inherited so the environment cannot choose it.
const SYSTEM_PATH: &str = "/usr/bin:/bin";

/// Bounds for one child process.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Wall-clock time from spawn until the child is killed.
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
    /// The program could not be started.
    #[error("could not start the process")]
    Spawn(#[source] io::Error),
    /// A thread to feed or drain the child could not be started.
    #[error("could not start a thread to feed or drain the process")]
    Thread(#[source] io::Error),
    /// The child ran past its deadline and was killed.
    #[error("the process did not finish before its deadline")]
    TimedOut,
    /// The child wrote more than the cap on stdout or stderr.
    #[error("the process wrote more output than allowed")]
    OutputTooLarge,
    /// Polling, feeding or draining the child failed.
    #[error("could not talk to the process")]
    Io(#[source] io::Error),
    /// A feeding or draining thread panicked; its payload is not an error and cannot be chained.
    #[error("a thread feeding or draining the process panicked")]
    ThreadPanicked,
    /// The process group had to be killed after `cause`, and the kill could not be carried
    /// out. The child itself was still killed and waited on.
    #[error("could not kill the process group after: {cause}")]
    Kill {
        /// The failure that made the kill necessary.
        cause: Box<ProcessError>,
        /// Why the group kill failed.
        #[source]
        source: io::Error,
    },
}

/// Runs `command` with `input` on stdin, within `limits`, and returns its output.
///
/// The child leads a new process group. stdin is closed when `input` is empty. When the child
/// or anything it started still runs or holds its pipes at the deadline, the whole group is
/// killed; the child is always waited on.
pub(crate) fn run(
    mut command: Command,
    input: &[u8],
    limits: Limits,
) -> Result<Captured, ProcessError> {
    let bounds = Bounds {
        deadline: Instant::now()
            .checked_add(limits.timeout)
            .ok_or(ProcessError::TimeoutTooLong)?,
        max_output_bytes: limits.max_output_bytes,
    };
    let stdin = if input.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    };
    command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(ProcessError::Spawn)?;

    let outcome = thread::scope(|scope| {
        let outcome = supervise(scope, &mut child, input, bounds);
        // Kill the group before the scope joins the feed and drain threads: they finish only
        // once every process holding the pipes is gone.
        outcome.map_err(|cause| match kill_group(&mut child) {
            Ok(()) => cause,
            Err(source) => ProcessError::Kill {
                cause: Box::new(cause),
                source,
            },
        })
    });
    let reaped = child.wait();
    match (outcome, reaped) {
        (Ok(captured), Ok(_)) => Ok(captured),
        (Ok(_), Err(source)) => Err(ProcessError::Io(source)),
        // After a kill, `wait` can only fail with ECHILD (already reaped); the cause is what
        // the caller must see, so a timeout stays a timeout.
        (Err(cause), _) => Err(cause),
    }
}

/// [`Limits`] resolved against the spawn time.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    deadline: Instant,
    max_output_bytes: usize,
}

/// Feeds `input`, drains both pipes and waits for the child until the deadline.
fn supervise<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    child: &mut Child,
    input: &'env [u8],
    bounds: Bounds,
) -> Result<Captured, ProcessError> {
    let cap = bounds.max_output_bytes;
    let feeder = child
        .stdin
        .take()
        .map(|stdin| spawn(scope, "child-stdin", move || feed(stdin, input)))
        .transpose()?;
    let stdout = child
        .stdout
        .take()
        .map(|pipe| spawn(scope, "child-stdout", move || drain(pipe, cap)))
        .transpose()?;
    let stderr = child
        .stderr
        .take()
        .map(|pipe| spawn(scope, "child-stderr", move || drain(pipe, cap)))
        .transpose()?;

    let status = poll_until(bounds.deadline, || child.try_wait())
        .map_err(ProcessError::Io)?
        .ok_or(ProcessError::TimedOut)?;
    // A grandchild can hold the pipes open after the child exits; joining would then wait
    // for it, so the threads must finish within the same deadline.
    let threads_done = || {
        let done = |handle: Option<bool>| handle.is_none_or(|finished| finished);
        done(feeder.as_ref().map(ScopedJoinHandle::is_finished))
            && done(stdout.as_ref().map(ScopedJoinHandle::is_finished))
            && done(stderr.as_ref().map(ScopedJoinHandle::is_finished))
    };
    poll_until(bounds.deadline, || Ok(threads_done().then_some(())))
        .map_err(ProcessError::Io)?
        .ok_or(ProcessError::TimedOut)?;

    let stdout = collect(stdout)?;
    let stderr = collect(stderr)?;
    if let Some(feeder) = feeder {
        feeder
            .join()
            .map_err(|_payload| ProcessError::ThreadPanicked)??;
    }
    Ok(Captured {
        status,
        stdout,
        stderr,
    })
}

/// Starts a named scoped thread (ASY-15).
fn spawn<'scope, T: Send + 'scope>(
    scope: &'scope Scope<'scope, '_>,
    name: &str,
    work: impl FnOnce() -> T + Send + 'scope,
) -> Result<ScopedJoinHandle<'scope, T>, ProcessError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn_scoped(scope, work)
        .map_err(ProcessError::Thread)
}

/// Kills the child's whole process group, then the child itself.
///
/// std cannot signal a group without `unsafe`, so this runs the system `kill` with a cleared
/// environment and a fixed `PATH`, itself bounded by [`KILL_TIMEOUT`]. `kill` exits non-zero
/// when no process is left in the group (the only failure possible for our own group), which
/// is the goal, so only a `kill` that cannot run or finish is an error.
fn kill_group(child: &mut Child) -> io::Result<()> {
    let group = signal_group(child.id());
    // Already-exited children report `Ok` here.
    let leader = child.kill();
    group.and(leader)
}

/// Sends SIGKILL to process group `group` through the system `kill`.
fn signal_group(group: u32) -> io::Result<()> {
    let mut kill = Command::new("kill");
    kill.env_clear()
        .env("PATH", SYSTEM_PATH)
        .args(["-s", "KILL", "--"])
        .arg(format!("-{group}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut killer = kill.spawn()?;
    let deadline = Instant::now()
        .checked_add(KILL_TIMEOUT)
        .ok_or_else(|| io::Error::other("kill deadline overflows the clock"))?;
    if poll_until(deadline, || killer.try_wait())?.is_some() {
        return Ok(());
    }
    killer.kill()?;
    killer.wait()?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "kill did not finish in time",
    ))
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

/// Writes all of `input` and closes stdin.
fn feed(mut stdin: impl Write, input: &[u8]) -> Result<(), ProcessError> {
    match stdin.write_all(input) {
        // A child that exits without reading all its input reports that through its exit
        // status, which the caller sees; the broken pipe adds nothing.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        written => written.map_err(ProcessError::Io),
    }
}

/// Reads `pipe` to its end, or until it holds more than `cap` bytes (`None`). Returning early
/// drops the pipe, so a child that keeps writing gets `EPIPE` and stops.
fn drain(pipe: impl Read, cap: usize) -> io::Result<Option<Vec<u8>>> {
    let limit = u64::try_from(cap).map_or(u64::MAX, |cap| cap.saturating_add(1));
    let mut buffer = Vec::new();
    pipe.take(limit).read_to_end(&mut buffer)?;
    Ok((buffer.len() <= cap).then_some(buffer))
}

/// Joins a drain thread and turns an over-cap read into [`ProcessError::OutputTooLarge`].
fn collect(
    drained: Option<ScopedJoinHandle<'_, io::Result<Option<Vec<u8>>>>>,
) -> Result<Vec<u8>, ProcessError> {
    let Some(handle) = drained else {
        return Ok(Vec::new());
    };
    handle
        .join()
        .map_err(|_payload| ProcessError::ThreadPanicked)?
        .map_err(ProcessError::Io)?
        .ok_or(ProcessError::OutputTooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENEROUS: Limits = Limits {
        timeout: Duration::from_secs(30),
        max_output_bytes: 1024,
    };

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn captures_stdout_and_exit_status() {
        let mut echo = Command::new("echo");
        echo.arg("hello");

        let output = run(echo, b"", GENEROUS).expect("echo runs");

        assert_eq!(output.stdout, b"hello\n", "stdout must be captured");
        assert!(output.status.success(), "echo exits 0");
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn feeds_input_to_stdin() {
        let output = run(Command::new("cat"), b"paths\0", GENEROUS).expect("cat runs");

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

        let result = run(sleeper, b"", limits);

        assert!(
            matches!(result, Err(ProcessError::TimedOut)),
            "a child past its deadline is a timeout, got {result:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn kills_a_grandchild_that_holds_the_output_pipe_past_the_deadline() {
        // The shell exits at once, leaving a background `sleep` holding stdout and stderr.
        // Waiting for those pipes to close would take 30 s and then succeed.
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let pid_file = scratch.path().join("grandchild.pid");
        let mut shell = Command::new("sh");
        shell
            .arg("-c")
            .arg(r#"sleep 30 & echo $! > "$0"; exit 0"#)
            .arg(&pid_file);
        let limits = Limits {
            timeout: Duration::from_millis(200),
            ..GENEROUS
        };

        let result = run(shell, b"", limits);

        assert!(
            matches!(result, Err(ProcessError::TimedOut)),
            "pipes held open past the deadline are a timeout, got {result:?}"
        );
        let pid = std::fs::read_to_string(&pid_file).expect("the shell recorded the grandchild");
        let state = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_owned))
            });
        assert!(
            state.as_deref().is_none_or(|state| state == "Z"),
            "the grandchild must have been killed with its process group, state {state:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn rejects_output_over_the_cap() {
        let mut yes = Command::new("yes");
        yes.arg("y");

        let result = run(yes, b"", GENEROUS);

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
        );

        assert!(
            matches!(result, Err(ProcessError::Spawn(_))),
            "a missing binary is a spawn failure, got {result:?}"
        );
    }
}
