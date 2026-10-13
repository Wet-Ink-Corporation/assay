//! Bounded child processes: a wall-clock deadline, capped output, a process group that is
//! killed rather than left behind, and a child that is always waited on (H-13).
//!
//! [`run`] supervises one child under [`Limits`]; [`GroupKiller::kill_group`] kills a process
//! group that another supervisor started. The git layer uses both, and `assay-cli` lends them
//! to the gate and model-runner adapters, which may not depend on this crate (wi-603c63).
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
//! is left, because the group id may then be reused; nor when its leader has been reaped and
//! `/proc` cannot be read, which is [`ProcessError::Kill`]. The remaining window is a member
//! exiting between the `/proc` scan and the signal (microseconds), after which an unrelated
//! process would have to take that id as its own group id before the signal lands.
//!
//! Runtime prerequisites: Linux `/proc` describing this process (not another pid namespace's)
//! and a procps or util-linux `kill` in `/usr/bin` or `/bin`. [`GroupKiller::system`] checks
//! both up front, and [`GroupKiller::at`] checks `/proc`, so a host without them fails closed
//! before any process starts.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running child, its output and its group are polled. Short against any timeout
/// we configure, long enough that polling costs nothing measurable.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Default deadline for a group kill, shared by the kill program's run and the group's exit.
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(5);

/// Where [`GroupKiller::system`] looks for `kill`, in order.
const SYSTEM_KILL: [&str; 2] = ["/usr/bin/kill", "/bin/kill"];

/// `PATH` for the kill program; fixed rather than inherited.
const SYSTEM_PATH: &str = "/usr/bin:/bin";

/// The `/proc` entry that [`GroupKiller::system`] checks describes this process.
const SELF_STAT: &str = "/proc/self/stat";

/// `errno` for "no such process", which reading `/proc/<pid>/stat` of a process that has
/// just exited can return.
const ESRCH: i32 = 3;

/// Bounds for one child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Wall-clock time from spawn until the child's group is killed.
    pub timeout: Duration,
    /// Most bytes accepted on stdout, and separately on stderr.
    pub max_output_bytes: usize,
}

/// What a finished child produced.
#[derive(Debug)]
pub struct Captured {
    /// How the child exited.
    pub status: ExitStatus,
    /// What it wrote on stdout, at most [`Limits::max_output_bytes`].
    pub stdout: Vec<u8>,
    /// What it wrote on stderr, at most [`Limits::max_output_bytes`].
    pub stderr: Vec<u8>,
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
    /// `/proc/self/stat` does not describe this process (for example a `/proc` mounted from
    /// another pid namespace), so process groups cannot be supervised.
    #[error("{SELF_STAT} does not describe this process: Linux /proc is a runtime prerequisite")]
    ProcUnavailable {
        /// What was wrong with `/proc/self/stat`.
        #[source]
        cause: ProcFault,
    },
}

/// Why `/proc/self/stat` does not describe this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProcFault {
    /// It could not be read; `NotFound` when the host has no `/proc`.
    #[error("it could not be read ({0})")]
    Unreadable(io::ErrorKind),
    /// It was read but does not have the documented `pid (comm) state ppid pgrp ...` shape,
    /// ending in a newline, that a `/proc` scan parses.
    #[error("it does not have the documented `pid (comm) state ppid pgrp ...` shape")]
    Malformed,
    /// It names the process with this pid instead, as a `/proc` from another pid namespace
    /// does.
    #[error("it names process {0}")]
    NamesAnotherProcess(u32),
}

/// The program that kills a timed-out child's whole process group: a procps or util-linux
/// compatible `kill` taking `-s KILL -- -<pgid>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupKiller {
    program: PathBuf,
    grace: Duration,
}

impl GroupKiller {
    /// The system `kill` from `/usr/bin` or `/bin`, with a five-second grace, once `/proc` is
    /// shown to describe this process.
    ///
    /// # Errors
    /// [`KillerError::ProcUnavailable`] when `/proc/self/stat` cannot be read or names another
    /// process; [`KillerError::NotFound`] when neither location holds an executable `kill`.
    pub fn system() -> Result<Self, KillerError> {
        check_proc(Path::new(SELF_STAT), std::process::id())?;
        SYSTEM_KILL
            .into_iter()
            .find_map(|program| Self::executable(program, KILL_GRACE).ok())
            .ok_or(KillerError::NotFound {
                searched: &SYSTEM_KILL,
            })
    }

    /// `program` as the kill program, once `/proc` is shown to describe this process. One
    /// `grace` deadline covers both its run and the group's exit after it.
    ///
    /// # Errors
    /// [`KillerError::ProcUnavailable`] when `/proc/self/stat` cannot be read or does not name
    /// this process; [`KillerError::NotExecutable`] when `program` is not an executable file.
    pub fn at(program: impl Into<PathBuf>, grace: Duration) -> Result<Self, KillerError> {
        Self::checked(Path::new(SELF_STAT), std::process::id(), program, grace)
    }

    /// [`at`](Self::at) with the `/proc/<pid>/stat` file `stat` expected to name `pid`.
    fn checked(
        stat: &Path,
        pid: u32,
        program: impl Into<PathBuf>,
        grace: Duration,
    ) -> Result<Self, KillerError> {
        check_proc(stat, pid)?;
        Self::executable(program, grace)
    }

    /// `program` as the kill program when it is an executable file; `/proc` is not checked.
    fn executable(program: impl Into<PathBuf>, grace: Duration) -> Result<Self, KillerError> {
        let program = program.into();
        let executable = fs::metadata(&program)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
        if executable {
            Ok(Self { program, grace })
        } else {
            Err(KillerError::NotExecutable { path: program })
        }
    }

    /// Kills every live member of process group `group` and returns once `/proc` shows none
    /// left, within the grace.
    ///
    /// `group` must be the id of a group that a child of this process leads (a child started
    /// with `process_group(0)` leads the group named by its pid). Nothing is signalled when
    /// `/proc` shows no live member: once the group is empty and its leader reaped, the id may
    /// belong to an unrelated process. The window left is a member exiting between that scan
    /// and the signal, after which an unrelated process would have to take the id as its own
    /// group id before the signal lands.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidInput`] for group 0 or 1, which `kill` reads as this process's
    /// own group and as every process, and for this process's pid or its own group, read from
    /// `/proc/self/stat` (which failing to read is an error too). Otherwise an error when `/proc` cannot be read (nothing
    /// is signalled), when the kill program cannot be run, or when a live member is still left
    /// at the end of the grace: then the group may still be running.
    pub fn kill_group(&self, group: u32) -> io::Result<()> {
        check_target(group)?;
        if group_alive(group)? {
            self.signal_group(group)
        } else {
            Ok(())
        }
    }

    /// Kills process group `group`, whose leader (the child with pid `group`) has not been
    /// reaped yet, and returns once `/proc` shows no live member left, within the grace.
    ///
    /// The unreaped leader holds the id, so it cannot have been reused: the group is signalled
    /// without first scanning `/proc` for a live member. Use [`kill_group`](Self::kill_group)
    /// once the leader may have been reaped.
    ///
    /// # Errors
    /// The same as [`kill_group`](Self::kill_group): [`io::ErrorKind::InvalidInput`] for a
    /// group that names this process or every process, and otherwise an error when the kill
    /// program cannot be run or a live member is still left at the end of the grace.
    pub fn kill_led_group(&self, group: u32) -> io::Result<()> {
        check_target(group)?;
        self.signal_group(group)
    }

    /// Sends SIGKILL to process group `group` and waits, within the grace, until no live
    /// member is left. The program's exit status is not believed either way.
    fn signal_group(&self, group: u32) -> io::Result<()> {
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
/// The child leads a new process group, and its stdin, stdout and stderr are anonymous
/// temporary files; its program, arguments, environment and working directory are the
/// caller's. The run completes only when the child has exited and no live process is left in
/// its group. On a timeout or an over-cap output the group is killed with `killer`; the child is
/// always waited on.
///
/// # Errors
/// [`ProcessError::TimedOut`] when the child or a process in its group was still running at
/// the deadline, and [`ProcessError::OutputTooLarge`] when it wrote more than the cap: in both
/// cases the group was killed and shown gone. [`ProcessError::Kill`] when the group could not
/// be shown gone, so it may still be running: abort the job rather than retry.
/// [`ProcessError::TimeoutTooLong`], [`ProcessError::Spool`] and [`ProcessError::Spawn`] when
/// the child could not be started, and [`ProcessError::Io`] when it could not be observed.
pub fn run(
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
        .map_err(|interrupted| terminate(&mut child, interrupted, killer, group_alive));
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
/// `alive` reports whether the group has a live member ([`group_alive`] outside tests).
fn terminate(
    child: &mut Child,
    interrupted: Interrupted,
    killer: &GroupKiller,
    alive: impl FnOnce(u32) -> io::Result<bool>,
) -> ProcessError {
    let Interrupted {
        cause,
        leader_reaped,
    } = interrupted;
    let group = child.id();
    // Once the leader is reaped the id may be reused: signal only a group seen to be alive.
    if leader_reaped {
        match alive(group) {
            Ok(true) => {}
            Ok(false) => return cause,
            Err(source) => {
                return ProcessError::Kill {
                    cause: Box::new(cause),
                    source,
                };
            }
        }
    }
    let group_killed = killer.signal_group(group);
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

/// Fails unless the `/proc/<pid>/stat` file at `stat` names `pid` and has the shape a `/proc`
/// scan parses ([`in_group`]): a missing `/proc`, one from another pid namespace, or one whose
/// stat lines the scan cannot read would make every group look empty or every scan fail.
fn check_proc(stat: &Path, pid: u32) -> Result<(), KillerError> {
    let line = fs::read(stat).map_err(|error| KillerError::ProcUnavailable {
        cause: ProcFault::Unreadable(error.kind()),
    })?;
    let malformed = KillerError::ProcUnavailable {
        cause: ProcFault::Malformed,
    };
    if in_group(&line, 0).is_err() {
        return Err(malformed);
    }
    let named: u32 = line
        .windows(2)
        .position(|pair| pair == b" (")
        .and_then(|pid_end| line.get(..pid_end))
        .and_then(|field| std::str::from_utf8(field).ok())
        .and_then(|field| field.parse().ok())
        .ok_or(malformed)?;
    if named == pid {
        Ok(())
    } else {
        Err(KillerError::ProcUnavailable {
            cause: ProcFault::NamesAnotherProcess(named),
        })
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
        if stat_names_live_member(&entry.path().join("stat"), group)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// [`is_live_member`] over the `/proc/<pid>/stat` file at `stat`, read as bytes because the
/// kernel writes `comm` unescaped and it need not be UTF-8.
fn stat_names_live_member(stat: &Path, group: u32) -> io::Result<bool> {
    is_live_member(|| fs::read(stat), group)
}

/// Whether the process whose `/proc/<pid>/stat` `read` returns is a live member of `group`.
///
/// A process that is gone (the read fails with `NotFound` or `ESRCH`) is not a member. A
/// process that exits while its stat is read can also yield an empty or truncated line, so an
/// unparseable line is read once more: gone on that re-read means it exited mid-scan, a
/// parseable line is used, and a line that is still unparseable is an `InvalidData` error,
/// because a present process that cannot be read must not make its group look empty.
fn is_live_member(mut read: impl FnMut() -> io::Result<Vec<u8>>, group: u32) -> io::Result<bool> {
    let Some(stat) = read_present(&mut read)? else {
        return Ok(false);
    };
    if let Ok(member) = in_group(&stat, group) {
        return Ok(member);
    }
    let Some(stat) = read_present(&mut read)? else {
        return Ok(false);
    };
    in_group(&stat, group).map_err(|UnreadableStat| {
        io::Error::new(io::ErrorKind::InvalidData, "unreadable /proc/<pid>/stat")
    })
}

/// One read of a `/proc/<pid>/stat`: `None` when the process is gone (`NotFound` or `ESRCH`).
fn read_present(read: &mut impl FnMut() -> io::Result<Vec<u8>>) -> io::Result<Option<Vec<u8>>> {
    match read() {
        Ok(stat) => Ok(Some(stat)),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(ESRCH) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// A `/proc/<pid>/stat` line that does not have the documented shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnreadableStat;

/// Whether a `/proc/<pid>/stat` line is a live (not zombie or dead) member of `group`; it is
/// parsed by [`state_and_group`].
fn in_group(stat: &[u8], group: u32) -> Result<bool, UnreadableStat> {
    let (state, pgrp) = state_and_group(stat)?;
    Ok(pgrp == group && state != "Z" && state != "X")
}

/// Fails with [`io::ErrorKind::InvalidInput`] unless `group` may be signalled: not 0 or 1
/// (`kill` reads `-0` as this process's group and `-1` as every process), not this process's
/// pid and not its own process group.
fn check_target(group: u32) -> io::Result<()> {
    let refused = |why: &str| Err(io::Error::new(io::ErrorKind::InvalidInput, why));
    if group <= 1 {
        return refused("process groups 0 and 1 name this process's group and every process");
    }
    if group == std::process::id() {
        return refused("the process group is led by this process");
    }
    let stat = fs::read(SELF_STAT)?;
    let (_, own) = state_and_group(&stat).map_err(|UnreadableStat| {
        io::Error::new(io::ErrorKind::InvalidData, "unreadable /proc/self/stat")
    })?;
    if group == own {
        return refused("the process group is this process's own");
    }
    Ok(())
}

/// The state (field 3) and process group (field 5) of a `/proc/<pid>/stat` line
/// (`pid (comm) state ppid pgrp ...`). `comm` may contain `) ` and any other bytes, so the
/// fields start after the last `) `, and only they must be text. A line without the kernel's
/// trailing `\n` was cut off, possibly inside `pgrp`, and is unreadable.
fn state_and_group(stat: &[u8]) -> Result<(&str, u32), UnreadableStat> {
    let line = stat.strip_suffix(b"\n").ok_or(UnreadableStat)?;
    let comm_end = line
        .windows(2)
        .rposition(|pair| pair == b") ")
        .ok_or(UnreadableStat)?;
    let fields = line
        .get(comm_end..)
        .and_then(|rest| rest.strip_prefix(b") "))
        .ok_or(UnreadableStat)?;
    let fields = std::str::from_utf8(fields).map_err(|_not_utf8| UnreadableStat)?;
    let mut fields = fields.split_whitespace();
    let state = fields.next().ok_or(UnreadableStat)?;
    let pgrp: u32 = fields
        .nth(1)
        .and_then(|pgrp| pgrp.parse().ok())
        .ok_or(UnreadableStat)?;
    Ok((state, pgrp))
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
    use std::io::BufRead;
    use std::os::unix::process::ExitStatusExt;

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
        // `run` returns only once `/proc` shows no live member of the group.
        assert!(
            is_dead(pid.trim()),
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
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn a_reaped_leader_whose_group_cannot_be_observed_is_not_signalled() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let marker = scratch.path().join("killed");
        let spy = script(
            scratch.path(),
            "kill",
            &format!("touch '{}'", marker.display()),
        );
        let killer = GroupKiller::at(&spy, Duration::from_millis(100)).expect("executable");
        let mut child = Command::new("true").spawn().expect("true starts");
        child.wait().expect("true is reaped");
        let interrupted = Interrupted {
            cause: ProcessError::TimedOut,
            leader_reaped: true,
        };

        let error = terminate(&mut child, interrupted, &killer, |_| {
            Err(io::Error::other("/proc is unreadable"))
        });

        assert!(
            matches!(&error, ProcessError::Kill { cause, .. } if matches!(**cause, ProcessError::TimedOut)),
            "an unobservable group must surface as a kill failure, got {error:?}"
        );
        assert!(
            !marker.exists(),
            "a group whose leader is reaped must not be signalled (pgid reuse)"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn kill_group_kills_every_live_member_of_the_group() {
        let mut shell = Command::new("sh");
        shell
            .args(["-c", "sleep 600 & echo $!; wait"])
            .stdout(Stdio::piped())
            .process_group(0);
        let mut child = shell.spawn().expect("sh starts");
        let mut grandchild = String::new();
        // Reading the pid is the readiness signal: the grandchild exists before the kill.
        io::BufReader::new(child.stdout.take().expect("stdout is piped"))
            .read_line(&mut grandchild)
            .expect("the shell prints the grandchild's pid");

        let killed = system_killer().kill_group(child.id());

        let status = child.wait().expect("the shell is reaped");
        assert_eq!(
            (killed.map_err(|error| error.kind()), status.signal()),
            (Ok(()), Some(9)),
            "the group leader dies of SIGKILL"
        );
        assert!(
            is_dead(grandchild.trim()),
            "the grandchild dies with its group"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn kill_group_signals_nothing_when_no_member_is_alive() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let marker = scratch.path().join("killed");
        let spy = script(
            scratch.path(),
            "kill",
            &format!("touch '{}'", marker.display()),
        );
        let killer = GroupKiller::at(&spy, Duration::from_millis(100)).expect("executable");
        let mut child = Command::new("true")
            .process_group(0)
            .spawn()
            .expect("true starts");
        child.wait().expect("true is reaped");

        let outcome = killer.kill_group(child.id());

        assert_eq!(
            outcome.map_err(|error| error.kind()),
            Ok(()),
            "an empty group is already gone"
        );
        assert!(
            !marker.exists(),
            "a group with no live member must not be signalled (pgid reuse)"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads /proc; Miri isolates the file system")]
    fn kill_group_refuses_the_ids_kill_reads_as_this_group_or_every_process() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let marker = scratch.path().join("killed");
        let spy = script(
            scratch.path(),
            "kill",
            &format!("touch '{}'", marker.display()),
        );
        let killer = GroupKiller::at(&spy, Duration::from_millis(100)).expect("executable");

        let refused = [0, 1].map(|group| killer.kill_group(group).map_err(|error| error.kind()));

        assert_eq!(
            refused,
            [
                Err(io::ErrorKind::InvalidInput),
                Err(io::ErrorKind::InvalidInput)
            ],
            "`kill -- -0` is this process's group and `-1` every process"
        );
        assert!(!marker.exists(), "nothing was signalled");
    }

    /// This process's process group, read independently of the code under test.
    fn own_pgrp() -> u32 {
        let stat = fs::read_to_string(SELF_STAT).expect("/proc/self/stat is readable");
        let (_, fields) = stat
            .rsplit_once(") ")
            .expect("the stat line names a command");
        fields
            .split_whitespace()
            .nth(2)
            .and_then(|pgrp| pgrp.parse().ok())
            .expect("field 5 is the process group")
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads /proc; Miri isolates the file system")]
    fn both_kills_refuse_this_process_and_its_own_group() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let marker = scratch.path().join("killed");
        let spy = script(
            scratch.path(),
            "kill",
            &format!("touch '{}'", marker.display()),
        );
        let killer = GroupKiller::at(&spy, Duration::from_millis(100)).expect("executable");
        let own = [0, 1, std::process::id(), own_pgrp()];

        let refused: Vec<_> = own
            .iter()
            .flat_map(|&group| [killer.kill_group(group), killer.kill_led_group(group)])
            .map(|killed| killed.map_err(|error| error.kind()))
            .collect();

        assert_eq!(
            refused,
            vec![Err(io::ErrorKind::InvalidInput); 8],
            "a kill must never reach assay itself, its group, or every process"
        );
        assert!(!marker.exists(), "nothing was signalled");
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn kill_led_group_kills_the_leader_and_its_grandchild() {
        let mut shell = Command::new("sh");
        shell
            .args(["-c", "sleep 600 & echo $!; wait"])
            .stdout(Stdio::piped())
            .process_group(0);
        let mut child = shell.spawn().expect("sh starts");
        let mut grandchild = String::new();
        io::BufReader::new(child.stdout.take().expect("stdout is piped"))
            .read_line(&mut grandchild)
            .expect("the shell prints the grandchild's pid");

        let killed = system_killer().kill_led_group(child.id());

        let status = child.wait().expect("the shell is reaped");
        assert_eq!(
            (killed.map_err(|error| error.kind()), status.signal()),
            (Ok(()), Some(9)),
            "the group leader dies of SIGKILL"
        );
        assert!(
            is_dead(grandchild.trim()),
            "the grandchild dies with its group"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns processes; Miri cannot")]
    fn kill_led_group_signals_without_looking_for_a_live_member() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let marker = scratch.path().join("killed");
        let spy = script(
            scratch.path(),
            "kill",
            &format!("touch '{}'", marker.display()),
        );
        let killer = GroupKiller::at(&spy, Duration::from_secs(5)).expect("executable");
        let mut child = Command::new("true")
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("true starts");
        // End of file on its stdout: the leader is exiting or a zombie, and nothing in its
        // group is alive, yet it is unreaped, so its id is still its group's.
        io::BufReader::new(child.stdout.take().expect("stdout is piped"))
            .read_line(&mut String::new())
            .expect("true closes its stdout");

        let outcome = killer.kill_led_group(child.id());

        child.wait().expect("true is reaped");
        assert_eq!(
            outcome.map_err(|error| error.kind()),
            Ok(()),
            "the leader is gone"
        );
        assert!(
            marker.exists(),
            "an unreaped leader's group is signalled unconditionally"
        );
    }

    #[test]
    fn this_hosts_proc_describes_this_process() {
        assert_eq!(
            check_proc(Path::new(SELF_STAT), std::process::id()),
            Ok(()),
            "the test host mounts its own /proc"
        );
    }

    #[test]
    fn a_proc_that_describes_another_process_is_unavailable() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let stat = scratch.path().join("stat");
        fs::write(&stat, "7 (assay) R 1 7 7 0\n").expect("write the stat line");

        assert_eq!(
            check_proc(&stat, 8),
            Err(KillerError::ProcUnavailable {
                cause: ProcFault::NamesAnotherProcess(7)
            }),
            "a /proc from another pid namespace must fail closed"
        );
        fs::write(&stat, "garbage").expect("write the stat line");
        assert_eq!(
            check_proc(&stat, 7),
            Err(KillerError::ProcUnavailable {
                cause: ProcFault::Malformed
            }),
            "an unreadable stat line must fail closed"
        );
    }

    #[test]
    fn a_proc_whose_stat_lines_the_scan_cannot_parse_is_unavailable() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let stat = scratch.path().join("stat");
        fs::write(&stat, "7 (assay) R 1 7 7 0").expect("write the stat line");

        assert_eq!(
            check_proc(&stat, 7),
            Err(KillerError::ProcUnavailable {
                cause: ProcFault::Malformed
            }),
            "a line without the kernel's trailing newline would fail every scan, so fail once here"
        );
    }

    #[test]
    fn a_proc_naming_this_process_by_a_non_utf8_name_is_available() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let stat = scratch.path().join("stat");
        fs::write(&stat, b"7 (a\xffb) R 1 7 7 0\n").expect("write the stat line");

        assert_eq!(
            check_proc(&stat, 7),
            Ok(()),
            "an executable whose file name is not UTF-8 still passes the preflight"
        );
    }

    #[test]
    fn a_missing_proc_is_unavailable() {
        let scratch = tempfile::tempdir().expect("a scratch directory");

        assert_eq!(
            check_proc(&scratch.path().join("absent"), std::process::id()),
            Err(KillerError::ProcUnavailable {
                cause: ProcFault::Unreadable(io::ErrorKind::NotFound)
            }),
            "a host without /proc must fail closed"
        );
    }

    #[test]
    fn an_explicit_kill_program_is_refused_when_proc_describes_another_process() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let stat = scratch.path().join("stat");
        fs::write(&stat, "7 (assay) R 1 7 7 0\n").expect("write the stat line");

        // A missing program too: `/proc` is checked first, so its fault is what surfaces.
        let killer = GroupKiller::checked(&stat, 8, scratch.path().join("absent"), KILL_GRACE);

        assert_eq!(
            killer,
            Err(KillerError::ProcUnavailable {
                cause: ProcFault::NamesAnotherProcess(7)
            }),
            "a killer could never see the group it signals, so none is built"
        );
    }

    #[test]
    fn an_explicit_kill_program_is_accepted_when_proc_describes_this_process() {
        let killer = GroupKiller::at("/bin/true", KILL_GRACE);

        assert_eq!(
            killer.map(|killer| killer.program),
            Ok(PathBuf::from("/bin/true")),
            "this host's /proc passes the preflight"
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

    /// [`is_live_member`] of group 42 over a stat file whose successive reads return `reads`,
    /// with an error reduced to its kind.
    fn member_after(reads: Vec<io::Result<&str>>) -> Result<bool, io::ErrorKind> {
        let mut reads = reads.into_iter();
        let read = move || {
            reads
                .next()
                .expect("the scan reads no more often than scripted")
                .map(|stat| stat.as_bytes().to_vec())
        };
        is_live_member(read, 42).map_err(|error| error.kind())
    }

    fn vanished() -> io::Error {
        io::Error::from(io::ErrorKind::NotFound)
    }

    #[test]
    fn an_unparseable_stat_whose_process_then_vanishes_is_skipped() {
        let member = member_after(vec![Ok(""), Err(vanished())]);

        assert_eq!(
            member,
            Ok(false),
            "a process that exited mid-scan is not a member"
        );
    }

    #[test]
    fn an_unparseable_stat_whose_process_is_still_present_fails_closed() {
        let member = member_after(vec![Ok(""), Ok("42 (sh")]);

        assert_eq!(
            member,
            Err(io::ErrorKind::InvalidData),
            "a stat that stays unparseable must not be read as an empty group"
        );
    }

    #[test]
    fn an_unparseable_stat_that_parses_on_the_re_read_is_used() {
        let member = member_after(vec![Ok(""), Ok("42 (sh) S 1 42 42 0\n")]);

        assert_eq!(member, Ok(true), "the re-read shows a live member");
    }

    #[test]
    fn a_readable_stat_is_read_once() {
        // One scripted read: a second read would hit the script's end and fail the test.
        let member = member_after(vec![Ok("9 (sh) S 1 42 42 0\n")]);

        assert_eq!(member, Ok(true), "a live member on the first read");
    }

    #[test]
    fn a_re_read_that_fails_otherwise_is_an_error() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);

        let member = member_after(vec![Ok(""), Err(denied)]);

        assert_eq!(
            member,
            Err(io::ErrorKind::PermissionDenied),
            "only a gone process ends the re-read quietly"
        );
    }

    #[test]
    fn a_stat_cut_off_inside_its_group_takes_the_re_read_path() {
        // "9 (sh) S 1 42 42 0\n" cut after "4" would otherwise name group 4, not 42.
        let member = member_after(vec![Ok("9 (sh) S 1 4"), Ok("9 (sh) S 1 42 42 0\n")]);

        assert_eq!(
            member,
            Ok(true),
            "a truncated line is re-read, not believed"
        );
    }

    #[test]
    fn a_stat_line_without_its_trailing_newline_is_unreadable() {
        assert_eq!(
            in_group(b"9 (sh) S 1 4", 4),
            Err(UnreadableStat),
            "a line cut off inside pgrp must not parse as a shorter group id"
        );
    }

    #[test]
    fn a_process_that_is_gone_before_its_stat_is_read_is_skipped() {
        let not_found = member_after(vec![Err(vanished())]);
        let no_such_process = member_after(vec![Err(io::Error::from_raw_os_error(ESRCH))]);

        assert_eq!(not_found, Ok(false), "NotFound means it exited");
        assert_eq!(no_such_process, Ok(false), "ESRCH means it exited");
    }

    #[test]
    fn any_other_read_failure_is_an_error() {
        let member = member_after(vec![Err(io::Error::from(io::ErrorKind::PermissionDenied))]);

        assert_eq!(
            member,
            Err(io::ErrorKind::PermissionDenied),
            "an unreadable /proc fails closed"
        );
    }

    #[test]
    fn a_stat_whose_command_name_is_not_utf8_is_read() {
        // Any process can name itself with arbitrary bytes (`prctl(PR_SET_NAME)`).
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let stat = scratch.path().join("stat");
        fs::write(&stat, b"42 (a\xffb) S 1 42 42 0\n").expect("write the stat line");

        let member = stat_names_live_member(&stat, 42).map_err(|error| error.kind());

        assert_eq!(
            member,
            Ok(true),
            "the fields after the command name are ASCII and still name the group"
        );
    }

    #[test]
    fn a_process_in_the_group_is_alive_unless_it_is_a_zombie() {
        assert_eq!(
            in_group(b"42 (sh) S 1 42 42 0\n", 42),
            Ok(true),
            "a sleeping member"
        );
        assert_eq!(
            in_group(b"43 (odd) name) R 1 42 42 0\n", 42),
            Ok(true),
            "a command name containing `) ` is skipped over"
        );
        assert_eq!(
            in_group(b"44 (sh) Z 1 42 42 0\n", 42),
            Ok(false),
            "a zombie"
        );
        assert_eq!(
            in_group(b"45 (sh) S 1 7 7 0\n", 42),
            Ok(false),
            "another group"
        );
        assert_eq!(
            in_group(b"garbage", 42),
            Err(UnreadableStat),
            "an unreadable stat line fails closed"
        );
    }
}
