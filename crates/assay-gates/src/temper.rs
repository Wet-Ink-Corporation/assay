//! The `builtin:temper` adapter in `gates-static`, zone Z2s: the decision-relevant temper checks
//! that need no compilation, out of a hostile `build.rs`'s reach
//! (`docs/architecture/03-contracts.md` §2.1 steps 1-3, ADR-12, 05 T4 and T5).
//!
//! 1. `raised`: the totals of `.claude/temper-baseline.json` at base and head; any count that
//!    rises, or any new entry, fails.
//! 2. `exception`: the base's pinned temper runs `node <pinned>/temper.mjs scan --all --json`
//!    over the head tree; every `ask` hit not named in base `.assay/exceptions.toml` fails.
//! 3. Gate-config diff: the keys of `.claude/temper.json` that steer temper, at base and head;
//!    any difference fails unless base admits the exact head value.
//!
//! The PR's vendored `.claude/temper/` is never run; its drift from the pinned copy is a
//! `major` finding. A file the checks need that is missing or unparsable, a scan that does not
//! exit 0 or 1, or an unreadable pinned copy is a [`TemperError`]: the gate errors, it never
//! passes.

mod baseline;
mod error;
mod exceptions;
mod findings;
mod gate_config;
mod report;
mod scan;
mod vendored;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assay_app::{Adapter, GateDocument, GateJob, GateRun, GateRunner, Plugin, Termination};
use assay_domain::{Finding, FindingsDocument, FindingsSchema, GateConclusion, Source, SourceKind};

pub use error::{TemperError, TemperSettingsError};

use self::baseline::Totals;
use self::exceptions::Allowlist;
use self::gate_config::TemperConfig;
use crate::capture::CaptureSupervisor;
use crate::environment::GateEnvironment;
use crate::exec::ProcessSpec;
use crate::revision::{FileContent, Revision, RevisionFiles, TreeFile};

/// The program the pinned temper runs under, looked up on the gate environment's `PATH`.
const NODE: &str = "node";

/// The pinned copy's entry point, relative to its root.
const SCRIPT: &str = "temper.mjs";

/// The scan's arguments after the script: a pure source scan of every `.rs` file, as JSON.
const SCAN: [&str; 3] = ["scan", "--all", "--json"];

/// Most bytes of scan output read (and, separately, of its stderr).
const MAX_SCAN_OUTPUT_BYTES: usize = 16 << 20;

/// Where and how the temper adapter runs in this job.
#[derive(Debug, Clone)]
pub struct TemperSettings {
    pinned: PathBuf,
    head_tree: PathBuf,
    scan_args: Vec<String>,
    scan_env: Vec<(OsString, OsString)>,
    run_key: String,
}

impl TemperSettings {
    /// Settings that run the pinned temper in `pinned` (its vendored layout: `temper.mjs`,
    /// `lib/`, `VERSION`, prepared from the base's pin) over the head tree `head_tree`, with
    /// `environment` plus git hardening, and write documents for run `run_key`.
    ///
    /// The scan also sees `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and
    /// `GIT_CEILING_DIRECTORIES` set to the head tree's parent, so temper's project discovery
    /// cannot find a repository above the tree. `head_tree` should not sit inside a Cargo
    /// workspace either, since temper falls back to the outermost `Cargo.toml` above it.
    ///
    /// # Errors
    ///
    /// [`TemperSettingsError`] when `pinned` is relative or not UTF-8, `head_tree` is relative
    /// or the root, or `run_key` is empty.
    pub fn new(
        pinned: impl Into<PathBuf>,
        head_tree: impl Into<PathBuf>,
        environment: &GateEnvironment,
        run_key: impl Into<String>,
    ) -> Result<Self, TemperSettingsError> {
        let pinned = pinned.into();
        if !pinned.is_absolute() {
            return Err(TemperSettingsError::RelativePinned { path: pinned });
        }
        let Some(script) = pinned.join(SCRIPT).to_str().map(str::to_owned) else {
            return Err(TemperSettingsError::NonUtf8Pinned { path: pinned });
        };
        let head_tree = head_tree.into();
        let ceiling = match head_tree.parent() {
            Some(parent) if head_tree.is_absolute() => parent.as_os_str().to_owned(),
            _ => return Err(TemperSettingsError::HeadTree { path: head_tree }),
        };
        let run_key = run_key.into();
        if run_key.is_empty() {
            return Err(TemperSettingsError::EmptyRunKey);
        }
        let mut scan_env = environment.vars().to_vec();
        scan_env.extend([
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CEILING_DIRECTORIES".into(), ceiling),
        ]);
        let scan_args = [script]
            .into_iter()
            .chain(SCAN.map(str::to_owned))
            .collect();
        Ok(Self {
            pinned,
            head_tree,
            scan_args,
            scan_env,
            run_key,
        })
    }
}

/// The temper adapter, generic over the supervisor that runs the pinned scan and the reader of
/// the PR's base and head files.
#[derive(Debug)]
pub struct TemperAdapter<S, R> {
    supervisor: S,
    revisions: R,
    settings: TemperSettings,
}

impl<S, R> TemperAdapter<S, R> {
    /// An adapter that runs the scan through `supervisor` and reads the PR through
    /// `revisions`. Pass `&mut supervisor` and `&revisions` to keep them.
    pub const fn new(supervisor: S, revisions: R, settings: TemperSettings) -> Self {
        Self {
            supervisor,
            revisions,
            settings,
        }
    }
}

/// The adapter's error for a supervisor `S` and a reader `R`.
type ErrorOf<S, R> = TemperError<<S as CaptureSupervisor>::Error, <R as RevisionFiles>::Error>;

impl<S: CaptureSupervisor, R: RevisionFiles> GateRunner for TemperAdapter<S, R> {
    type Error = ErrorOf<S, R>;

    /// Runs the Z2s checks and reports them as one `assay.findings/1` document that concludes
    /// `fail` when `raised`, `exception` or the gate-config diff found anything.
    ///
    /// # Errors
    ///
    /// [`TemperError`] without running anything when `plugin` is not temper or `job` is not
    /// `gates-static`; and when a file the checks need is missing, not a regular file or
    /// unparsable, the scan cannot run, does not exit 0 or 1 or prints unreadable output, or
    /// the pinned copy cannot be read.
    fn run(&mut self, plugin: &Plugin, job: GateJob) -> Result<GateRun, Self::Error> {
        let Adapter::Temper { pin, .. } = plugin.adapter() else {
            return Err(TemperError::NotTemper {
                plugin: plugin.name().as_str().to_owned(),
            });
        };
        if job != GateJob::Static {
            return Err(TemperError::OutsideStatic { job });
        }
        let pin = pin.to_string();
        let allowlist = self.allowlist()?;
        let mut failing = self.raised()?;
        failing.extend(self.gate_config(&allowlist)?);
        failing.extend(findings::exceptions(
            self.scan(plugin.timeout())?,
            &allowlist,
        ));
        let drift = self.vendored_drift(&pin)?;
        let conclusion = if failing.is_empty() {
            GateConclusion::Pass
        } else {
            GateConclusion::Fail
        };
        failing.extend(drift);
        let document = FindingsDocument {
            schema: FindingsSchema::V1,
            source: Source {
                kind: SourceKind::Gate,
                name: plugin.name().as_str().to_owned(),
                version: pin,
                role: None,
                // The settings keep the key for the next run; the document owns its copy.
                run_key: self.settings.run_key.clone(),
            },
            conclusion: Some(conclusion),
            findings: failing,
            stats: None,
        };
        let bytes = serde_json::to_vec(&document).map_err(TemperError::Document)?;
        Ok(GateRun {
            termination: Termination::Exited(0),
            document: GateDocument::Written(bytes),
        })
    }
}

impl<S: CaptureSupervisor, R: RevisionFiles> TemperAdapter<S, R> {
    /// Step 1: the baseline totals diff.
    fn raised(&self) -> Result<Vec<Finding>, ErrorOf<S, R>> {
        let base = self.required(Revision::Base, baseline::PATH)?;
        let head = self.required(Revision::Head, baseline::PATH)?;
        let parse = |revision, bytes: &[u8]| {
            Totals::parse(bytes).map_err(|detail| malformed(revision, baseline::PATH, detail))
        };
        let (before, after) = (parse(Revision::Base, &base)?, parse(Revision::Head, &head)?);
        let raises = baseline::raises(&before, &after);
        Ok(findings::raised(&raises, str::from_utf8(&head).ok()))
    }

    /// Base `.assay/exceptions.toml`; an absent file admits nothing.
    fn allowlist(&self) -> Result<Allowlist, ErrorOf<S, R>> {
        let Some(bytes) = self.read(Revision::Base, exceptions::PATH)? else {
            return Ok(Allowlist::default());
        };
        str::from_utf8(&bytes)
            .map_err(|_utf8| "not UTF-8".to_owned())
            .and_then(Allowlist::parse)
            .map_err(|detail| malformed(Revision::Base, exceptions::PATH, detail))
    }

    /// Step 3: the gate-config diff.
    fn gate_config(&self, allowlist: &Allowlist) -> Result<Vec<Finding>, ErrorOf<S, R>> {
        let base = self.required(Revision::Base, gate_config::PATH)?;
        let head = self.required(Revision::Head, gate_config::PATH)?;
        let parse = |revision, bytes: &[u8]| {
            TemperConfig::parse(bytes)
                .map_err(|detail| malformed(revision, gate_config::PATH, detail))
        };
        let (before, after) = (parse(Revision::Base, &base)?, parse(Revision::Head, &head)?);
        let drifted = gate_config::drifted(&before, &after, allowlist);
        Ok(findings::gate_config(
            &drifted,
            str::from_utf8(&base).ok(),
            str::from_utf8(&head).ok(),
        ))
    }

    /// Step 2: the exception requests of the pinned scan over the head tree.
    fn scan(&mut self, timeout: Duration) -> Result<Vec<scan::Request>, ErrorOf<S, R>> {
        let spec = ProcessSpec {
            program: Path::new(NODE),
            args: &self.settings.scan_args,
            cwd: &self.settings.head_tree,
            env: &self.settings.scan_env,
            timeout,
            max_output_bytes: MAX_SCAN_OUTPUT_BYTES,
        };
        let run = self
            .supervisor
            .capture(&spec)
            .map_err(TemperError::Supervisor)?;
        match run.termination {
            Termination::Exited(0 | 1) => {}
            termination => return Err(TemperError::ScanEnded { termination }),
        }
        scan::requests(&run.stdout).map_err(|detail| TemperError::ScanOutput { detail })
    }

    /// The vendored copy's drift from the pinned copy.
    fn vendored_drift(&self, pin: &str) -> Result<Option<Finding>, ErrorOf<S, R>> {
        let pinned = vendored::read_pinned(&self.settings.pinned)
            .map_err(|(path, source)| TemperError::Pinned { path, source })?;
        let head = self
            .revisions
            .files(Revision::Head, vendored::DIR)
            .map_err(|source| TemperError::Revision {
                revision: Revision::Head,
                path: vendored::DIR,
                source,
            })?;
        let drifted: Vec<_> = vendored::drift(&pinned, head).into_iter().collect();
        Ok(findings::drift(&drifted, pin))
    }

    /// The bytes of the regular file at `path` in `revision`, or an error when it is absent.
    fn required(&self, revision: Revision, path: &'static str) -> Result<Vec<u8>, ErrorOf<S, R>> {
        self.read(revision, path)?
            .ok_or(TemperError::Missing { revision, path })
    }

    /// The bytes of the regular file at `path` in `revision`, `None` when nothing is there, and
    /// an error when something other than one regular file is.
    fn read(
        &self,
        revision: Revision,
        path: &'static str,
    ) -> Result<Option<Vec<u8>>, ErrorOf<S, R>> {
        let mut files =
            self.revisions
                .files(revision, path)
                .map_err(|source| TemperError::Revision {
                    revision,
                    path,
                    source,
                })?;
        let only = files.pop();
        match (only, files.is_empty()) {
            (None, _) => Ok(None),
            (
                Some(TreeFile {
                    path: found,
                    content: FileContent::Bytes(bytes),
                }),
                true,
            ) if found == path => Ok(Some(bytes)),
            _ => Err(TemperError::NotAFile { revision, path }),
        }
    }
}

/// A [`TemperError::Malformed`] for `path` at `revision`.
const fn malformed<S, R>(
    revision: Revision,
    path: &'static str,
    detail: String,
) -> TemperError<S, R> {
    TemperError::Malformed {
        revision,
        path,
        detail,
    }
}
