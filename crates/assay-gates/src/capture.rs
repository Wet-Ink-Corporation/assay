//! A bounded process run whose stdout is kept: how the temper adapter runs the pinned
//! `temper scan --all --json`, which prints its findings rather than writing a file.

use assay_app::Termination;

use crate::exec::ProcessSpec;

/// How a captured process ended and what it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRun {
    /// How the process ended.
    pub termination: Termination,
    /// Its stdout, at most the spec's `max_output_bytes`; empty unless it exited.
    pub stdout: Vec<u8>,
}

/// Runs one process to completion within bounds and keeps its stdout.
///
/// The same duties as [`Supervisor`](crate::Supervisor): the child leads its own process group
/// and gets exactly `spec.env`, its output is spooled rather than piped, the whole group is
/// killed at the deadline or past the output cap, and it is always waited on (H-13).
pub trait CaptureSupervisor {
    /// Why the process could not be run or could not be shown to be gone. `'static` because
    /// it becomes the `source` of an error.
    type Error: std::error::Error + 'static;

    /// Runs `spec` and reports how the process ended and what it printed to stdout.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when the process could not be started, observed or killed.
    fn capture(&mut self, spec: &ProcessSpec<'_>) -> Result<CapturedRun, Self::Error>;
}

impl<S: CaptureSupervisor + ?Sized> CaptureSupervisor for &mut S {
    type Error = S::Error;

    fn capture(&mut self, spec: &ProcessSpec<'_>) -> Result<CapturedRun, Self::Error> {
        (**self).capture(spec)
    }
}
