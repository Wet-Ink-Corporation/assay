//! A bounded `claude` process: a wall-clock deadline, a capped transcript, and a child that is
//! killed and waited on rather than left behind (H-13).
//!
//! stdin and stdout are anonymous temporary files, not pipes: the child reads its input from a
//! file written in full before it starts, and writes its transcript to another, so no writer or
//! reader can block on it. stderr is closed. The calling thread polls the child and the size of
//! its output against one deadline. Past the deadline or the cap, the child is killed and reaped.
//!
//! The child leads its own process group, but only the child itself is killed: the group killer
//! that also stops descendants lives in `assay-forge`, which this crate may not depend on
//! (ADR-07). With no process-spawning tool (`--tools Read,Grep,Glob`, `--restricted`) the CLI's
//! only descendants are its own helpers, which exit once the CLI is gone.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::HeadlessError;
use crate::stream::Ending;

/// How often the child and its output are checked.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A finished call: how it stopped and what it wrote (at most the cap).
#[derive(Debug)]
pub(super) struct Captured {
    pub(super) ending: Ending,
    pub(super) transcript: Vec<u8>,
}

/// Why polling stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watched {
    /// The child exited and was reaped.
    Exited,
    /// The child must be killed.
    Stop(Ending),
}

/// Runs `command` with `input` on its stdin until it exits, `timeout` passes or its stdout
/// passes `cap` bytes.
pub(super) fn run(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
    cap: u64,
) -> Result<Captured, HeadlessError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(HeadlessError::TimeoutTooLong { timeout })?;
    let stdin = input_file(input).map_err(HeadlessError::SpoolCreate)?;
    let mut stdout = tempfile::tempfile().map_err(HeadlessError::SpoolCreate)?;
    let child_end = stdout.try_clone().map_err(HeadlessError::SpoolCreate)?;
    command
        .stdin(stdin)
        .stdout(child_end)
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().map_err(HeadlessError::Spawn)?;
    // Closes this process's copy of the child's stdout.
    drop(command);

    let ending = match watch(&mut child, &stdout, cap, deadline) {
        Ok(Watched::Exited) => {
            if size(&stdout).map_err(HeadlessError::Supervise)? > cap {
                Ending::OutputCapped
            } else {
                Ending::Exited
            }
        }
        Ok(Watched::Stop(ending)) => {
            stop(&mut child)?;
            ending
        }
        Err(source) => {
            // A failed kill outranks the polling failure: the CLI may still be running.
            stop(&mut child)?;
            return Err(HeadlessError::Supervise(source));
        }
    };
    let transcript = read_capped(&mut stdout, cap).map_err(HeadlessError::ReadBack)?;
    Ok(Captured { ending, transcript })
}

/// An anonymous temporary file holding `input`, positioned at its start.
fn input_file(input: &[u8]) -> io::Result<File> {
    let mut file = tempfile::tempfile()?;
    file.write_all(input)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

/// Polls until the child exits, its output passes `cap`, or `deadline` passes.
fn watch(child: &mut Child, stdout: &File, cap: u64, deadline: Instant) -> io::Result<Watched> {
    loop {
        if size(stdout)? > cap {
            return Ok(Watched::Stop(Ending::OutputCapped));
        }
        if child.try_wait()?.is_some() {
            return Ok(Watched::Exited);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(Watched::Stop(Ending::TimedOut));
        }
        thread::sleep(left.min(POLL_INTERVAL));
    }
}

/// Kills the child and reaps it. When the kill fails the child is not waited on, since the wait
/// could be unbounded; the error says it may still be running.
fn stop(child: &mut Child) -> Result<(), HeadlessError> {
    child.kill().map_err(HeadlessError::Kill)?;
    child.wait().map_err(HeadlessError::Supervise)?;
    Ok(())
}

fn size(file: &File) -> io::Result<u64> {
    Ok(file.metadata()?.len())
}

/// The first `cap` bytes of `file`.
fn read_capped(file: &mut File, cap: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut transcript = Vec::new();
    file.take(cap).read_to_end(&mut transcript)?;
    Ok(transcript)
}
