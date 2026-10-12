//! The `exec` adapter: runs a plugin's PR-controlled command in its Z2b job
//! (`docs/architecture/03-contracts.md` §1, §2).
//!
//! The command runs from the head checkout with only the [`GateEnvironment`] plus
//! [`FINDINGS_OUT_VAR`], which names a file in a directory created fresh for this run, so a
//! document left by an earlier run can never be read. The process itself is run by a
//! [`Supervisor`], which owns the timeout, the output cap and the process group. The document
//! is read only if it is a regular file (never through a symlink) and no larger than the cap.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{
    Adapter, BuildJob, GateDocument, GateJob, GateRun, GateRunner, Plugin, Termination,
};
use thiserror::Error;

use crate::environment::GateEnvironment;

/// The variable naming the file a gate writes its `assay.findings/1` document to.
pub const FINDINGS_OUT_VAR: &str = "ASSAY_FINDINGS_OUT";

/// The document's file name inside the run's output directory.
pub const FINDINGS_FILE: &str = "findings.json";

/// Runs one process to completion within bounds.
///
/// An implementation must make the child lead its own process group, give it exactly
/// `spec.env`, spool its output rather than pipe it, kill the whole group at the deadline or
/// past the output cap, and always wait on it (H-13).
pub trait Supervisor {
    /// Why the process could not be run or could not be shown to be gone. `'static` because
    /// it becomes the `source` of an [`ExecError`], and `Error::source` hands out
    /// `&(dyn Error + 'static)`.
    type Error: std::error::Error + 'static;

    /// Runs `spec` and reports how the process ended.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when the process could not be started, observed or killed.
    fn run(&mut self, spec: &ProcessSpec<'_>) -> Result<Termination, Self::Error>;
}

impl<S: Supervisor + ?Sized> Supervisor for &mut S {
    type Error = S::Error;

    fn run(&mut self, spec: &ProcessSpec<'_>) -> Result<Termination, Self::Error> {
        (**self).run(spec)
    }
}

/// One process to run.
#[derive(Debug, Clone, Copy)]
pub struct ProcessSpec<'a> {
    /// The program: a bare name looked up on the `PATH` in `env`, or a path.
    pub program: &'a Path,
    /// Its arguments.
    pub args: &'a [String],
    /// Its working directory.
    pub cwd: &'a Path,
    /// Its whole environment; nothing else is inherited.
    pub env: &'a [(OsString, OsString)],
    /// Wall-clock time from start until its process group is killed.
    pub timeout: Duration,
    /// Most bytes accepted on stdout, and separately on stderr.
    pub max_output_bytes: usize,
}

/// Size caps for an exec gate run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecLimits {
    /// Most bytes the process may write to stdout, and separately to stderr.
    pub max_output_bytes: usize,
    /// Largest document read, in bytes; a larger one is [`GateDocument::TooLarge`].
    pub max_document_bytes: u64,
}

impl ExecLimits {
    /// 16 MiB of output per stream and an 8 MiB document.
    pub const DEFAULT: Self = Self {
        max_output_bytes: 16 << 20,
        max_document_bytes: 8 << 20,
    };
}

/// Where and how exec gates run in this job. Both directories are absolute, so neither depends
/// on this process's working directory.
#[derive(Debug, Clone)]
pub struct ExecSettings {
    checkout: PathBuf,
    output_root: PathBuf,
    environment: GateEnvironment,
    limits: ExecLimits,
}

/// Why [`ExecSettings`] were refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecSettingsError {
    /// The head checkout is a relative path.
    #[error("the checkout {} is not an absolute path", path.display())]
    RelativeCheckout {
        /// The refused path.
        path: PathBuf,
    },
    /// The output root is a relative path.
    #[error("the output root {} is not an absolute path", path.display())]
    RelativeOutputRoot {
        /// The refused path.
        path: PathBuf,
    },
}

impl ExecSettings {
    /// Settings that run commands from `checkout` (the head checkout), give each run a fresh
    /// `<output_root>/<plugin>/` (`output_root` must exist), pass `environment` and enforce
    /// `limits`.
    ///
    /// # Errors
    ///
    /// [`ExecSettingsError::RelativeCheckout`] or [`ExecSettingsError::RelativeOutputRoot`]
    /// when either directory is a relative path.
    pub fn new(
        checkout: impl Into<PathBuf>,
        output_root: impl Into<PathBuf>,
        environment: GateEnvironment,
        limits: ExecLimits,
    ) -> Result<Self, ExecSettingsError> {
        let checkout = checkout.into();
        if !checkout.is_absolute() {
            return Err(ExecSettingsError::RelativeCheckout { path: checkout });
        }
        let output_root = output_root.into();
        if !output_root.is_absolute() {
            return Err(ExecSettingsError::RelativeOutputRoot { path: output_root });
        }
        Ok(Self {
            checkout,
            output_root,
            environment,
            limits,
        })
    }
}

/// The `exec` adapter, generic over the process supervisor that bounds the run.
#[derive(Debug)]
pub struct ExecAdapter<S> {
    supervisor: S,
    settings: ExecSettings,
}

impl<S> ExecAdapter<S> {
    /// An adapter that runs processes through `supervisor`. Pass `&mut supervisor` to keep it.
    pub const fn new(supervisor: S, settings: ExecSettings) -> Self {
        Self {
            supervisor,
            settings,
        }
    }
}

impl<S: Supervisor> GateRunner for ExecAdapter<S> {
    type Error = ExecError<S::Error>;

    /// Runs an `exec` plugin's command in its own Z2b job.
    ///
    /// # Errors
    ///
    /// Returns [`ExecError`] without running anything when `plugin` is not an `exec` plugin,
    /// `job` is not a Z2b job or not the plugin's job, or the run's output directory cannot be
    /// created fresh; and when the supervisor fails or the document cannot be inspected.
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        let Adapter::Exec { command, job: own } = plugin.adapter() else {
            return Err(ExecError::NotExec {
                plugin: plugin.name().as_str().to_owned(),
            });
        };
        let GateJob::Build(job) = job else {
            return Err(ExecError::OutsideBuildJob);
        };
        if job != *own {
            return Err(ExecError::WrongJob {
                plugin: plugin.name().as_str().to_owned(),
                job,
            });
        }
        let output = self.settings.output_root.join(plugin.name().as_str());
        fs::create_dir(&output).map_err(|source| ExecError::OutputDirectory {
            path: output.clone(),
            source,
        })?;
        let document = output.join(FINDINGS_FILE);
        let env = self.environment(&document);
        let program = resolve(&self.settings.checkout, command.program());
        let spec = ProcessSpec {
            program: &program,
            args: command.args(),
            cwd: &self.settings.checkout,
            env: &env,
            timeout: plugin.timeout(),
            max_output_bytes: self.settings.limits.max_output_bytes,
        };
        let termination = self.supervisor.run(&spec).map_err(ExecError::Supervisor)?;
        let document = read_document(&document, self.settings.limits.max_document_bytes).map_err(
            |source| ExecError::ReadDocument {
                path: document,
                source,
            },
        )?;
        Ok(GateRun {
            termination,
            document,
        })
    }
}

impl<S> ExecAdapter<S> {
    /// The allowlisted variables, then this run's findings path.
    fn environment(&self, document: &Path) -> Vec<(OsString, OsString)> {
        // The allowlist never holds an inherited `ASSAY_FINDINGS_OUT` (only the six names of
        // 03 §2), so this run's path is the only one.
        let mut env = self.settings.environment.vars().to_vec();
        env.push((FINDINGS_OUT_VAR.into(), document.into()));
        env
    }
}

/// `program` as the process should be started: a path with a directory part is taken relative
/// to the checkout, a bare name is left for the `PATH` lookup.
fn resolve(checkout: &Path, program: &str) -> PathBuf {
    let path = Path::new(program);
    if path.is_relative() && path.components().nth(1).is_some() {
        checkout.join(path)
    } else {
        path.to_path_buf()
    }
}

/// Reads the document at `path` without following a symlink and without reading past `cap`.
///
/// The type check and the open are two calls. The supervisor has already shown the gate's
/// process group gone, so only a process that left the group (`setsid`) could swap the file in
/// between; the read stays capped, but a FIFO swapped in could block the open. Closing that
/// window needs `O_NOFOLLOW | O_NONBLOCK`, which std cannot name without `libc` (H-16).
fn read_document(path: &Path, cap: u64) -> io::Result<GateDocument> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(GateDocument::Missing),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Ok(GateDocument::NotAFile);
    }
    if metadata.len() > cap {
        return Ok(GateDocument::TooLarge);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let over = u64::try_from(bytes.len()).map_or(true, |len| len > cap);
    Ok(if over {
        GateDocument::TooLarge
    } else {
        GateDocument::Written(bytes)
    })
}

/// Why the exec adapter did not produce a [`GateRun`]. The `gate` use case records each as
/// `gate-error:not-run`.
#[derive(Debug, Error)]
pub enum ExecError<E> {
    /// The plugin's adapter is not `exec`.
    #[error("plugin `{plugin}` is not an exec gate")]
    NotExec {
        /// The plugin.
        plugin: String,
    },
    /// The job is `gates-static` (Z2s), where no PR-controlled command may run.
    #[error("exec gates run only in a Z2b job, never in gates-static")]
    OutsideBuildJob,
    /// The job is a Z2b job other than the plugin's own.
    #[error("plugin `{plugin}` does not run in {job}")]
    WrongJob {
        /// The plugin.
        plugin: String,
        /// The job asked for.
        job: BuildJob,
    },
    /// The run's output directory could not be created, or already existed.
    #[error("could not create the output directory {}", path.display())]
    OutputDirectory {
        /// The directory.
        path: PathBuf,
        /// The file system's error.
        #[source]
        source: io::Error,
    },
    /// The supervisor could not run the process.
    #[error("could not run the exec gate")]
    Supervisor(#[source] E),
    /// The document's path could not be inspected or read.
    #[error("could not read the gate document {}", path.display())]
    ReadDocument {
        /// The document's path.
        path: PathBuf,
        /// The file system's error.
        #[source]
        source: io::Error,
    },
}
