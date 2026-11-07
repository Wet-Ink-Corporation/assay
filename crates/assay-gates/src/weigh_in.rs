//! The `builtin:weigh-in` adapter in experimental mode (`docs/architecture/04-pipeline.md` §9,
//! 03 §2.2, #109, #372). It runs the base's pinned `weighin ci-check --json` (D-WI's command,
//! Weigh-In 1.1.0 and later) and never links Weigh-In (P8), reads its `weighin.ci-check/1`
//! report strictly (`weigh_in/report.rs`), and reports as warnings 04 §9 rule 1's two sets, open
//! triaged items that block (`weigh-in-open-blocking`) and open items not triaged yet
//! (`weigh-in-pending-triage`), and each journal line rule 5's append-only check found removed,
//! missing, altered or undecodable (`weigh-in-journal-rewritten`). It runs non-required only and
//! never fails the gate; rule 2's corroboration and rule 3's evidence rule are not implemented,
//! and G2 is enforced by CP1 and by convention meanwhile.
//!
//! `ci-check` reads the journals from the working tree and the history (the range's commits,
//! trailers, atoms) from git, so it runs in a reading checkout of the head commit whose history
//! reaches the base (`assay_forge::git::ObjectRepo::reading_checkout`), as
//! `ci-check --json --base <base> --dir <tree> --store <store>` with the tree as working
//! directory and exactly the gate environment. `--store` names a path in the job's scratch, so
//! the PR's `.kb/_ledger/config.json` cannot choose the ledger file. `ci-check` exits 0
//! whenever it wrote its report (CLI-06); any other ending, and a report assay does not read,
//! is a [`WeighInError`] (`gate-error:not-run`), never a partial read (wi-b5affa).

mod error;
mod findings;
mod id;
mod report;

use std::ffi::OsString;
use std::path::PathBuf;

use assay_app::{
    Adapter, CommitId, GateDocument, GateJob, GateRun, GateRunner, MAX_DOCUMENT_BYTES, Pin, Plugin,
    Termination,
};
use assay_domain::{FindingsDocument, FindingsSchema, GateConclusion, Source, SourceKind};

pub use error::{WeighInError, WeighInSettingsError};
pub use report::WeighInReportError;

use crate::capture::CaptureSupervisor;
use crate::environment::GateEnvironment;
use crate::exec::ProcessSpec;

/// What the pinned `weighin ci-check` runs on in this job. Every path is absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiCheck {
    /// The pinned `weighin` binary; never looked up on `PATH`.
    pub program: PathBuf,
    /// The reading checkout of the head commit, `ci-check`'s working directory and `--dir`.
    pub tree: PathBuf,
    /// `--store`: a ledger file path in the job's scratch (it need not exist), so the PR's
    /// ledger config cannot choose one.
    pub store: PathBuf,
    /// The PR's pinned base commit, `--base`; the report must be about it.
    pub base: CommitId,
    /// The PR's pinned head commit, checked out in `tree`; the report must be about it.
    pub head: CommitId,
}

/// Where and how the Weigh-In adapter runs in this job.
#[derive(Debug, Clone)]
pub struct WeighInSettings {
    program: PathBuf,
    tree: PathBuf,
    args: Vec<String>,
    env: Vec<(OsString, OsString)>,
    base: CommitId,
    head: CommitId,
    run_key: String,
}

impl WeighInSettings {
    /// Settings that run `<program> ci-check --json --base <base> --dir <tree> --store <store>`
    /// in `tree` with exactly `environment`, and write documents for run `run_key`.
    ///
    /// # Errors
    ///
    /// [`WeighInSettingsError`] when the program, the tree or the store is relative, the tree or
    /// the store is not UTF-8, `environment`'s `PATH` has an empty or relative entry (which
    /// `ci-check`'s own git would resolve from the PR's tree), or `run_key` is empty.
    pub fn new(
        check: CiCheck,
        environment: &GateEnvironment,
        run_key: impl Into<String>,
    ) -> Result<Self, WeighInSettingsError> {
        let CiCheck {
            program,
            tree,
            store,
            base,
            head,
        } = check;
        if !program.is_absolute() {
            return Err(WeighInSettingsError::RelativeProgram { path: program });
        }
        if !tree.is_absolute() {
            return Err(WeighInSettingsError::RelativeTree { path: tree });
        }
        if !store.is_absolute() {
            return Err(WeighInSettingsError::RelativeStore { path: store });
        }
        if !environment.search_path_is_absolute() {
            return Err(WeighInSettingsError::RelativeSearchPath);
        }
        let run_key = run_key.into();
        if run_key.is_empty() {
            return Err(WeighInSettingsError::EmptyRunKey);
        }
        let (Some(dir), Some(store_arg)) = (tree.to_str(), store.to_str()) else {
            let path = if tree.to_str().is_none() { tree } else { store };
            return Err(WeighInSettingsError::NotUtf8 { path });
        };
        let args = ["ci-check", "--json", "--base", base.as_str(), "--dir", dir]
            .into_iter()
            .chain(["--store", store_arg])
            .map(str::to_owned)
            .collect();
        Ok(Self {
            program,
            tree,
            args,
            env: environment.vars().to_vec(),
            base,
            head,
            run_key,
        })
    }
}

/// The Weigh-In adapter, generic over the supervisor that runs `ci-check` and keeps its stdout.
#[derive(Debug)]
pub struct WeighInAdapter<S> {
    supervisor: S,
    settings: WeighInSettings,
}

impl<S> WeighInAdapter<S> {
    /// An adapter that runs `ci-check` through `supervisor` (pass `&mut supervisor` to keep it)
    /// with `settings`.
    #[must_use]
    pub const fn new(supervisor: S, settings: WeighInSettings) -> Self {
        Self {
            supervisor,
            settings,
        }
    }
}

impl<S: CaptureSupervisor> WeighInAdapter<S> {
    /// The adapter's refusals, which [`GateRunner::run`] makes first, without running
    /// anything: a caller that must prepare what `ci-check` reads (the reading checkout)
    /// checks them before it does.
    ///
    /// # Errors
    ///
    /// [`WeighInError::NotWeighIn`] when `plugin` is not weigh-in, [`WeighInError::Required`]
    /// when it is required, and [`WeighInError::OutsideStatic`] when `job` is not
    /// `gates-static`.
    pub fn admit(plugin: &Plugin, job: GateJob) -> Result<(), WeighInError<S::Error>> {
        admitted(plugin, job).map(|_| ())
    }
}

/// `plugin`'s pin when the adapter runs it in `job`; the refusals of [`WeighInAdapter::admit`]
/// otherwise.
fn admitted<E>(plugin: &Plugin, job: GateJob) -> Result<&Pin, WeighInError<E>> {
    let Adapter::WeighIn { pin } = plugin.adapter() else {
        return Err(WeighInError::NotWeighIn {
            plugin: plugin.name().as_str().to_owned(),
        });
    };
    if plugin.required() {
        return Err(WeighInError::Required {
            plugin: plugin.name().as_str().to_owned(),
        });
    }
    if job != GateJob::Static {
        return Err(WeighInError::OutsideStatic { job });
    }
    Ok(pin)
}

impl<S: CaptureSupervisor> GateRunner for WeighInAdapter<S> {
    type Error = WeighInError<S::Error>;

    /// Runs `ci-check` and reports the blocking and pending-triage sets and every journal
    /// violation of its report as one `assay.findings/1` document that concludes `pass` when
    /// it reports nothing and `neutral` otherwise; never `fail`.
    ///
    /// # Errors
    ///
    /// [`WeighInError`] without running anything when `plugin` is not weigh-in, is required,
    /// or `job` is not `gates-static`; when `ci-check` cannot be run, does not exit 0, or
    /// prints a report that is refused.
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        let pin = admitted(plugin, job)?;
        let settings = &self.settings;
        let spec = ProcessSpec {
            program: &settings.program,
            args: &settings.args,
            cwd: &settings.tree,
            env: &settings.env,
            timeout: plugin.timeout(),
            max_output_bytes: MAX_DOCUMENT_BYTES,
        };
        let run = self
            .supervisor
            .capture(&spec)
            .map_err(WeighInError::Supervisor)?;
        if run.termination != Termination::Exited(0) {
            return Err(WeighInError::Ended {
                termination: run.termination,
            });
        }
        let report = report::read(&run.stdout, &settings.base, &settings.head)?;
        let findings = findings::warnings(&report);
        let conclusion = if findings.is_empty() {
            GateConclusion::Pass
        } else {
            GateConclusion::Neutral
        };
        let document = FindingsDocument {
            schema: FindingsSchema::V1,
            source: Source {
                kind: SourceKind::Gate,
                name: plugin.name().as_str().to_owned(),
                version: pin.to_string(),
                role: None,
                // The settings keep the key for the next run; the document owns its copy.
                run_key: settings.run_key.clone(),
            },
            conclusion: Some(conclusion),
            findings,
            // Weigh-In does not attest step results (#153): `decide` reads its gate as unknown.
            failed_steps: None,
            stats: None,
        };
        let bytes = serde_json::to_vec(&document).map_err(WeighInError::Document)?;
        Ok(GateRun {
            termination: Termination::Exited(0),
            document: GateDocument::Written(bytes),
        })
    }
}
