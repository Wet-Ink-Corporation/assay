//! The plugin's declared steps (`[[plugin.step]]`), run in their Z2b job from the head tree
//! (`docs/architecture/03-contracts.md` §1, §2.1 step 7): each under its own `timeout_s`, with
//! the gate environment. Shared by `gates-lint` (`fmt`, `deny`) and `gates-test` (`tests`,
//! `doctests`); both retry a step once when its run ended in an infra-class way (F9,
//! `06-operations.md` §4). [`TemperStepSettings`] is what every step needs, and all that
//! `gates-test` needs.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{GateDocument, GateRun, Plugin, Step, Termination};
use assay_domain::{Finding, FindingsDocument, FindingsSchema, GateConclusion, Source, SourceKind};

use super::{ErrorOf, TemperAdapter, TemperError, TemperSettingsError};
use crate::capture::CaptureSupervisor;
use crate::environment::GateEnvironment;
use crate::exec::{ProcessSpec, resolve};
use crate::revision::RevisionFiles;

/// Most bytes of stdout (and, separately, stderr) of each step and of `baseline --check`.
const MAX_STEP_OUTPUT_BYTES: usize = 16 << 20;

/// Where the temper adapter runs processes and which run its documents belong to.
///
/// That is the head tree, the gate environment with git hardening, and the run key.
/// [`TemperSettings`](super::TemperSettings) holds one, and
/// [`TemperTestAdapter`](super::TemperTestAdapter) needs nothing else.
#[derive(Debug, Clone)]
pub struct TemperStepSettings {
    pub(super) head_tree: PathBuf,
    /// The gate environment plus git hardening, for the scan and every declared step.
    pub(super) env: Vec<(OsString, OsString)>,
    run_key: String,
}

impl TemperStepSettings {
    /// Settings that run processes from `head_tree` with `environment` plus
    /// `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CEILING_DIRECTORIES` set
    /// to the head tree's parent, and write documents for run `run_key`.
    ///
    /// # Errors
    ///
    /// [`TemperSettingsError`] when the head tree is relative or the root, `environment`'s
    /// `PATH` has an empty or relative entry (a child's own `git` would resolve it from the
    /// head tree), or `run_key` is empty.
    pub fn new(
        head_tree: PathBuf,
        environment: &GateEnvironment,
        run_key: impl Into<String>,
    ) -> Result<Self, TemperSettingsError> {
        let ceiling = match head_tree.parent() {
            Some(parent) if head_tree.is_absolute() => parent.as_os_str().to_owned(),
            _ => return Err(TemperSettingsError::HeadTree { path: head_tree }),
        };
        if !search_path_is_absolute(environment) {
            return Err(TemperSettingsError::RelativeSearchPath);
        }
        let run_key = run_key.into();
        if run_key.is_empty() {
            return Err(TemperSettingsError::EmptyRunKey);
        }
        let mut env = environment.vars().to_vec();
        env.extend([
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CEILING_DIRECTORIES".into(), ceiling),
        ]);
        Ok(Self {
            head_tree,
            env,
            run_key,
        })
    }

    /// The run's `assay.findings/1` document: `plugin` at `pin` concludes `conclusion` with
    /// `findings`, as a clean exit.
    pub(super) fn document<SE, RE>(
        &self,
        plugin: &Plugin,
        pin: String,
        (conclusion, findings): (GateConclusion, Vec<Finding>),
    ) -> Result<GateRun, TemperError<SE, RE>> {
        let document = FindingsDocument {
            schema: FindingsSchema::V1,
            source: Source {
                kind: SourceKind::Gate,
                name: plugin.name().as_str().to_owned(),
                version: pin,
                role: None,
                // The settings keep the key for the next run; the document owns its copy.
                run_key: self.run_key.clone(),
            },
            conclusion: Some(conclusion),
            findings,
            stats: None,
        };
        let bytes = serde_json::to_vec(&document).map_err(TemperError::Document)?;
        Ok(GateRun {
            termination: Termination::Exited(0),
            document: GateDocument::Written(bytes),
        })
    }
}

/// Whether every entry of `environment`'s `PATH` is an absolute directory. An empty entry
/// means the working directory, here the head tree.
fn search_path_is_absolute(environment: &GateEnvironment) -> bool {
    environment
        .vars()
        .iter()
        .filter(|(name, _)| name == "PATH")
        .all(|(_, value)| env::split_paths(value).all(|entry| entry.is_absolute()))
}

/// A process run that ended without exiting: the gate use case reports its termination.
pub(super) const fn stopped(termination: Termination) -> GateRun {
    GateRun {
        termination,
        document: GateDocument::Missing,
    }
}

/// Whether a step's run ended in an infra-class way (F9): a signal it did not exit from, which
/// comes from outside the step (the runner, the kernel's OOM killer), since a test that crashes
/// is reported by the test runner's own exit code. Nothing else is: an exit code is the step's
/// verdict (cargo exits 101 for a registry failure and a compile error alike, and reading its
/// stderr would model its text), a timeout or an output cap would recur, and a supervisor error
/// may leave the process running (`ProcessError::Kill`: abort, never retry).
const fn infra_class(termination: Termination) -> bool {
    matches!(termination, Termination::Signalled)
}

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// [`step_retried`] through this adapter's supervisor and settings.
    pub(super) fn step_retried(&mut self, step: &Step) -> Result<Termination, ErrorOf<S, R>> {
        step_retried(&mut self.supervisor, &self.settings.steps, step)
    }
}

/// Runs one declared step once, from the head tree with the gate environment under its own
/// timeout.
fn run_step<S: CaptureSupervisor, R>(
    supervisor: &mut S,
    settings: &TemperStepSettings,
    step: &Step,
) -> Result<Termination, TemperError<S::Error, R>> {
    let program = resolve(&settings.head_tree, step.command().program());
    capture(
        supervisor,
        settings,
        (
            step.name().as_str(),
            program.as_path(),
            step.command().args(),
        ),
        step.timeout(),
    )
}

/// Runs one declared step like [`run_step`], and once more when the first run was infra-class;
/// the last run's termination decides. At most two runs, so at most twice the step's
/// `timeout_s`.
pub(super) fn step_retried<S: CaptureSupervisor, R>(
    supervisor: &mut S,
    settings: &TemperStepSettings,
    step: &Step,
) -> Result<Termination, TemperError<S::Error, R>> {
    let first = run_step(supervisor, settings, step)?;
    if infra_class(first) {
        run_step(supervisor, settings, step)
    } else {
        Ok(first)
    }
}

/// Runs `(process, program, args)` from the head tree with the gate environment under
/// `timeout`, once; `process` names it in errors.
pub(super) fn capture<S: CaptureSupervisor, R>(
    supervisor: &mut S,
    settings: &TemperStepSettings,
    (process, program, args): (&str, &Path, &[String]),
    timeout: Duration,
) -> Result<Termination, TemperError<S::Error, R>> {
    let spec = ProcessSpec {
        program,
        args,
        cwd: &settings.head_tree,
        env: &settings.env,
        timeout,
        max_output_bytes: MAX_STEP_OUTPUT_BYTES,
    };
    supervisor
        .capture(&spec)
        .map(|run| run.termination)
        .map_err(|source| TemperError::Lint {
            process: process.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_signal_is_infra_class() {
        let terminations = [
            Termination::Exited(0),
            Termination::Exited(1),
            Termination::Exited(4),
            Termination::Exited(101),
            Termination::Signalled,
            Termination::TimedOut,
            Termination::OutputTooLarge,
        ];

        let infra: Vec<Termination> = terminations
            .into_iter()
            .filter(|termination| infra_class(*termination))
            .collect();

        assert_eq!(infra, [Termination::Signalled]);
    }
}
