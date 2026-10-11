//! Bounded child processes: a wall-clock deadline, capped output, and a child that is always
//! waited on (H-13).
//!
//! stdout and stderr are drained by scoped threads while the calling thread polls the child
//! against its deadline. Every path that leaves [`run`] after a successful spawn has waited on
//! the child, killing it first when it has not exited on its own.

use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

/// How often a running child is polled for exit. Short against any timeout we configure, long
/// enough that polling costs nothing measurable.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

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
    /// The child had to be killed after `cause`, and killing it failed, so it was not waited on.
    #[error("could not kill the process after: {cause}")]
    Kill {
        /// The failure that made the kill necessary.
        cause: Box<ProcessError>,
        /// Why the kill failed.
        #[source]
        source: io::Error,
    },
}

/// Runs `command` with `input` on stdin, within `limits`, and returns its output.
///
/// stdin is closed when `input` is empty. A child that is still running at the deadline is
/// killed and waited on.
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
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(ProcessError::Spawn)?;

    let outcome = thread::scope(|scope| {
        let outcome = supervise(scope, &mut child, input, bounds);
        // Kill before the scope joins the drain threads: they only finish once the pipes close.
        match outcome {
            Ok(captured) => Ok(captured),
            Err(cause) => match child.kill() {
                Ok(()) => Err(cause),
                Err(source) => Err(ProcessError::Kill {
                    cause: Box::new(cause),
                    source,
                }),
            },
        }
    });
    if let Err(ProcessError::Kill { .. }) = outcome {
        // Waiting on a child that could not be killed could block forever.
        return outcome;
    }
    // Reaps the child; after a successful `try_wait` this returns the cached status at once.
    child.wait().map_err(ProcessError::Io)?;
    outcome
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

    let status = wait_until(child, bounds.deadline)?.ok_or(ProcessError::TimedOut)?;

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

/// Polls `child` until it exits (`Some`) or `deadline` passes (`None`).
fn wait_until(child: &mut Child, deadline: Instant) -> Result<Option<ExitStatus>, ProcessError> {
    loop {
        let Some(left) = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
        else {
            return Ok(None);
        };
        if let Some(status) = child.try_wait().map_err(ProcessError::Io)? {
            return Ok(Some(status));
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
        let started = Instant::now();

        let result = run(sleeper, b"", limits);

        assert!(
            matches!(result, Err(ProcessError::TimedOut)),
            "a child past its deadline is a timeout, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the runner must not wait for the child to finish on its own"
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
